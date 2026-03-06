# ZeroClaw 멀티 에이전트 아키텍처 설계안

## 개요

ZeroClaw의 멀티 에이전트 아키텍처는 현재 2단계 구축 중에 있으며, 단일 에이전트 모델에서 진화하는 고급 협력 시스템을 구현합니다. 이 설계안은 ZeroClaw의 핵심 엔지니어링 원칙을 따르면서 확장 가능하고 안정적인 멀티 에이전트 환경을 제공합니다.

### 현재 상태
- **Phase 1**: In-Process Delegation (기능 완료)
- **Phase 2**: File-Based Multi-Agent Architecture (개발 중)
- **핵심 시스템**: 에이전트 레지스트리, 보안 경계, 기본 위임

---

## 1. 설계 철학 및 원칙

### 핵심 엔지니어링 원칙 적용

| 원칙 | 멀티 에이전트 적용 | 설계 지침 |
|------|-------------------|-----------|
| **KISS** | 단순 통신 프로토콜 | 요청/응답 패턴, 상태 비저장 통신 |
| **YAGNI** | 필수적 기능만 구현 | 기본 위임, 간단한 결과 집합, 타임아웃 |
| **DRY+3** | 공유 추출의 규칙 | 3번 사용 후 추상화, 단순 로직은 중복 |
| **SRP+ISP** | 단일 책임 에이전트 | ResearchAgent, CodeAgent, TestAgent 분리 |
| **Fail Fast** | 명시적 오류 처리 | 즉시 유효성 검사, 구조화된 오류 타입 |
| **Secure** | 최소 권한 원칙 | 에이전트별 도구 접근 제어, 자원 제한 |
| **Deterministic** | 결정적 실행 | 순차적 작업, 재현 가능한 결과 집합 |
| **Reversible** | 롤백 우선 사고 | 트랜잭션 실행, 상태 추적 |

---

## 2. 아키텍처 구조

### 2.1 통신 계층 구조

```
┌─────────────────────────────────────────────────────────────┐
│                 Application Layer                           │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐        │
│  │   Research  │  │    Code     │  │    Test     │        │
│  │   Agent     │  │   Agent     │  │   Agent     │        │
│  └─────────────┘  └─────────────┘  └─────────────┘        │
└─────────────────────────────────────────────────────────────┘
│                     Message Bus Layer                        │
│  ┌─────────────────────────────────────────────────────────┐ │
│  │                 NATS/Redis Pub/Sub                    │ │
│  │  • Request-Response • Publish-Subscribe • Event Streams │ │
│  └─────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
│                    Transport Layer                          │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐        │
│  │   gRPC      │  │   WebSocket │  │   Unix Sock │        │
│  │  (Binary)   │  │   (JSON)    │  │   (Local)   │        │
│  └─────────────┘  └─────────────┘  └─────────────┘        │
└─────────────────────────────────────────────────────────────┘
```

### 2.2 핵심 구성 요소

#### 에이전트 인터페이스
```rust
// 단일 책임 에이전트 인터페이스
trait ResearchAgent {
    fn research(&self, topic: &str) -> Result<String>;
    fn synthesize(&self, sources: Vec<String>) -> Result<String>;
}

trait CodeAgent {
    fn generate_code(&self, spec: &str) -> Result<String>;
    fn refactor_code(&self, code: &str) -> Result<String>;
    fn optimize_code(&self, code: &str) -> Result<String>;
}

trait TestAgent {
    fn run_tests(&self, code: &str) -> Result<TestResults>;
    fn validate_output(&self, output: &str, expected: &str) -> Result<bool>;
}
```

#### 메시지 형식 표준
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub id: String,                    // UUIDv4
    pub from: AgentIdentity,          // 발신자 정보
    pub to: Vec<AgentIdentity>,       // 수신자 목록
    pub message_type: MessageType,    // 메시지 타입
    pub payload: MessagePayload,      // 실제 데이터
    pub timestamp: DateTime<Utc>,     // 타임스탬프
    pub correlation_id: Option<String>, // 상관 ID
}

pub enum MessageType {
    Request { action: String, parameters: Value },
    Response { success: bool, result: Value, error: Option<String> },
    Event { event_type: String, data: Value },
    TaskAssignment { task_id: String, agent_id: String },
}
```

---

## 3. 작업 분배 및 오케스트레이션

### 3.1 작업 큐 시스템

```rust
pub struct TaskQueue {
    pub priority_queue: PriorityQueue<Task, TaskPriority>,
    pub fair_queue: FairQueue<Task>,
    pub scheduled_queue: ScheduledQueue<Task>,
}

#[derive(Debug, Clone)]
pub struct Task {
    pub id: String,
    pub name: String,
    pub description: String,
    pub priority: TaskPriority,
    pub dependencies: Vec<TaskId>,
    pub required_capabilities: Vec<String>,
    pub estimated_duration: Duration,
    pub deadline: Option<DateTime<Utc>>,
    pub retry_policy: RetryPolicy,
}
```

### 3.2 워크플로우 오케스트레이션

```rust
pub struct WorkflowExecutor {
    pub workflow: Workflow,
    pub state: WorkflowState,
    pub execution_history: Vec<ExecutionStep>,
    pub coordinator: Coordinator,
}

pub struct Workflow {
    pub id: String,
    pub nodes: Vec<WorkflowNode>,
    pub edges: Vec<WorkflowEdge>,
    pub start_node: String,
    pub end_nodes: Vec<String>,
    pub error_handling: ErrorHandlingPolicy,
}
```

### 3.2 실행 패턴

| 패턴 | 설명 | 적용 사례 |
|------|------|-----------|
| **순차 실행** | 작업이 순서대로 실행 | Research → Code → Test 파이프라인 |
| **병렬 실행** | 여러 에이전트 동시 실행 | 독립적인 분석 작업 |
| **조건 실행** | 이전 결과에 따라 실행 | 성공/실패 기반 분기 |
| **반복 실행** | 조건이 만족될 때까지 반복 | 테스트 반복, 최적화 |

---

## 4. 통신 프로토콜

### 4.1 통신 패턴

```rust
pub enum RoutingPattern {
    // 단일 대상 통신
    Direct { target: AgentIdentity },

    // 방송 통신 (1:N)
    Broadcast { topic: String, exclude: Vec<AgentIdentity> },

    // 수집 통신 (N:1)
    Collect { aggregator: AgentIdentity, timeout: Duration },

    // 파이프라인 통신
    Pipeline { stages: Vec<StageConfig> },

    // 그룹 통신
    Group { group_id: String, pattern: GroupPattern },
}
```

### 4.2 상태 동기화

```rust
pub struct StateManager {
    pub state_store: Arc<StateStore>,
    pub sync_engine: SyncEngine,
    pub conflict_resolver: ConflictResolver,
}

pub enum ConsistencyLevel {
    최종 일관성 (eventual),
    강한 일관성 (strong),
    인과적 일관성 (causal),
}
```

---

## 5. 보안 및 접근 제어

### 5.1 에이전트 권한 시스템

```rust
pub struct AgentPermissions {
    pub allowed_tools: Vec<String>,
    pub allowed_domains: Vec<String>,
    pub max_memory_mb: u64,
    pub max_execution_time: u64,
    pub file_access_scope: FileAccessScope,
}

pub enum FileAccessScope {
    None,
    ReadOnly,
    WorkspaceOnly,
    Full,
}
```

### 5.2 보안 검증

```rust
impl SecurityPolicy {
    pub fn check_agent_permissions(&self, agent: &str, action: &str) -> Result<()> {
        let agent_perms = self.get_agent_permissions(agent)?;

        match action {
            "file_read" => {
                if !agent_perms.allowed_tools.contains(&"file_read".to_string()) {
                    bail!("에이전트 '{}'는 파일 읽기 권한이 없습니다", agent);
                }
            },
            "shell_execute" => {
                if !agent_perms.allowed_tools.contains(&"shell".to_string()) {
                    bail!("에이전트 '{}'는 쉘 실행 권한이 없습니다", agent);
                }
            },
            _ => bail!("알 수 없는 동작: {}", action),
        }

        Ok(())
    }
}
```

---

## 6. 오류 처리 및 복구

### 6.1 오류 타입 정의

```rust
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("에이전트 '{agent}'를 찾을 수 없음: {reason}")]
    AgentNotFound { agent: String, reason: String },

    #[error("위임 깊이 초과: {current}/{max}")]
    DepthExceeded { current: u32, max: u32 },

    #[error("작업 타임아웃: {timeout}초")]
    Timeout { timeout: u64 },

    #[error("권한 거부: {action} on {resource}")]
    PermissionDenied { action: String, resource: String },
}
```

### 6.2 복구 메커니즘

```rust
pub enum RecoveryStrategy {
    // 재시도 전략
    Retry {
        max_attempts: usize,
        backoff: BackoffStrategy,
        condition: RetryCondition
    },

    // 롤백 전략
    Rollback {
        checkpoint: Checkpoint,
        rollback_level: RollbackLevel
    },

    // 페일오버 전략
    Failover {
        backup_target: AgentIdentity,
        transfer_mode: TransferMode
    },

    // 서킷 브레이커
    CircuitBreaker {
        threshold: usize,
        timeout: Duration,
        half_open_threshold: usize
    },
}
```

---

## 7. 성능 최적화

### 7.1 로드 밸런싱

```rust
pub enum BalancingStrategy {
    라운드 로빈,
    가중 라운드 로빈,
    최소 연결,
    최소 응답 시간,
    사용자 정의,
}
```

### 7.2 캐싱 및 최적화

```rust
pub struct CacheManager {
    pub result_cache: Arc<RwLock<HashMap<String, CachedResult>>>,
    pub metadata_cache: Arc<RwLock<HashMap<String, Metadata>>>,
    pub eviction_policy: EvictionPolicy,
}
```

---

## 8. 구현 로드맵

### 단기 구현 (1-3개월)
1. **기본 통신 프로토콜**: 요청/응답 패턴 구현
2. **에이전트 레지스트리 개선**: 동적 발견 시스템
3. **간단한 협력 패턴**: 마스터-워커 구현
4. **보안 경계**: 최소 권한 시스템

### 중기 구현 (3-6개월)
1. **워크플로우 오케스트레이션**: DAG 기반 실행 엔진
2. **고급 통신 패턴**: 이벤트 기반 통신
3. **상태 관리**: 분산 상태 동기화
4. **오류 복구**: 자동 재시드 및 롤백

### 장기 구현 (6-12개월)
1. **자율적 에이전트**: 자기 조직 능력
2. **복잡한 협력 패턴**: � 및 마켓플레이스
3. **클러스터 수준 확장**: 다중 노드 분산
4. **AI 기반 최적화**: 자원 자동 관리

---

## 9. 예제 시나리오

### 시나리오 1: Research → Code → Test 파이프라인

```rust
// 워크플로우 정의
let workflow = Workflow {
    id: "development_pipeline".to_string(),
    nodes: vec![
        WorkflowNode {
            id: "research".to_string(),
            agent_id: "research-agent".to_string(),
            input: vec!["requirements".to_string()],
            output: vec!["research_output".to_string()],
        },
        WorkflowNode {
            id: "code".to_string(),
            agent_id: "code-agent".to_string(),
            input: vec!["research_output".to_string()],
            output: vec!["code_output".to_string()],
        },
        WorkflowNode {
            id: "test".to_string(),
            agent_id: "test-agent".to_string(),
            input: vec!["code_output".to_string()],
            output: vec!["test_results".to_string()],
        },
    ],
    edges: vec![
        WorkflowEdge::from("research").to("code"),
        WorkflowEdge::from("code").to("test"),
    ],
    start_node: "research".to_string(),
    end_nodes: vec!["test".to_string()],
};

// 실행
let executor = WorkflowExecutor::new(workflow);
let result = executor.execute().await?;
```

### 시나리오 2: 병렬 분석 작업

```rust
// 여러 에이전트에게 동시에 작업 할당
let tasks = vec![
    Task::new("analyze_tech_stack", "기술 스택 분석"),
    Task::new("analyze_market", "시장 분석"),
    Task::new("analyze_competitors", "경쟁사 분석"),
];

let results = task_queue.execute_parallel(tasks).await?;

// 결과 집합
let aggregated = ResultAggregator::merge(results)?;
```

---

## 10. 검증 및 테스트 전략

### 10.1 테스트 범위

| 테스트 유형 | 범위 | 목표 |
|------------|------|------|
| 단위 테스트 | 개별 에이전트 기능 | 각 에이전트의 정확성 |
| 통합 테스트 | 에이전트 간 통신 | 통신 프로토콜 검증 |
| 워크플로우 테스트 | 전체 파이프라인 | 엔드투엔드 동작 확인 |
| 부하 테스트 | 동시 에이전트 실행 | 성능 및 확장성 검증 |
| 장애 테스트 | 다양한 실패 시나리오 | 복구 메커니즘 검증 |

### 10.2 자동화 검증

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_agent_communication() {
        let (researcher, coder) = setup_test_agents();

        let result = researcher.research("Rust async programming").await;
        assert!(result.is_ok());

        let code = coder.generate_code(&result.unwrap()).await;
        assert!(code.is_ok());
    }

    #[tokio::test]
    async fn test_workflow_execution() {
        let workflow = create_test_workflow();
        let executor = WorkflowExecutor::new(workflow);

        let result = executor.execute().await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().status, WorkflowStatus::Completed);
    }
}
```

---

## 11. 롤아웃 전략

### 11.1 단계적 배포

1. **개발 환경**: 기능 검증 및 안정성 테스트
2. **스테이징 환경**: 성능 테스트 및 부하 검증
3. **프로덕션 환경**: 점진적 롤아웃 및 모니터링

### 11.2 롤백 계획

- **빠른 롤백**: 이전 버전으로 즉시 복귀
- **상태 롤백**: 데이터베이스 상태 복원
- **구성 롤백**: 설정 파일 복원
- **에이전트 롤백**: 개별 에이전트 복원

---

## 12. 모니터링 및 관리

### 12.1 메트릭 수집

```rust
pub struct MetricsCollector {
    pub counters: HashMap<String, AtomicU64>,
    pub gauges: HashMap<String, AtomicF64>,
    pub histograms: HashMap<String, Histogram>,
    pub meters: HashMap<String, Meter>,
}

// 주요 메트릭
pub const AGENT_TASK_COUNT: &str = "agent.tasks.total";
pub const AGENT_SUCCESS_RATE: &str = "agent.success.rate";
pub const AGENT_AVG_RESPONSE_TIME: &str = "agent.response.time.avg";
pub const AGENT_ERROR_COUNT: &str = "agent.errors.total";
```

### 12.2 경고 시스템

```rust
pub struct AlertManager {
    pub rules: Vec<AlertRule>,
    pub notification_channels: Vec<NotificationChannel>,
}

pub enum AlertLevel {
    Info,
    Warning,
    Error,
    Critical,
}
```

---

## 결론

이 멀티 에이전트 아키텍처 설계는 ZeroClaw의 엔지니어링 원칙을 준수하면서 확장 가능하고 안정적인 에이전트 협력 환경을 제공합니다. 기존의 단일 에이전트 모델에서 진화하여 복잡한 워크플로우, 자동 복구, 효율적인 자원 관리를 지원하며, 점진적 구현을 통해 안전한 롤아웃이 가능합니다.

이 설계를 통해 ZeroClaw는 단순한 에이전트 런타임을 넘어 **지능적인 협력 시스템**으로 발전할 수 있습니다.