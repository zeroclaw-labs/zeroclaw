#!/usr/bin/env bash
# ============================================================================
#  晶莱华科 (Geneline Bioscience) — OpenClaw Medical Skills 部署脚本
#  基于企业业务画像定制，分三阶段渐进式导入
#  生成日期: 2026-03-22
# ============================================================================

set -euo pipefail

# ---------------------- 配置区 ----------------------
# OpenClaw 用户: ~/.openclaw/skills/
# NanoClaw 用户: 改为你的容器技能目录，如 /path/to/nanoclaw/container/skills/
SKILLS_TARGET="${OPENCLAW_SKILLS_DIR:-$HOME/.zeroclaw/workspace/skills}"
REPO_DIR="OpenClaw-Medical-Skills/skills"

# 颜色输出
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
GRAY='\033[0;90m'
NC='\033[0m'

info()  { echo -e "${CYAN}[INFO]${NC}  $1"; }
ok()    { echo -e "${GREEN}[  OK]${NC}  $1"; }
warn()  { echo -e "${YELLOW}[WARN]${NC}  $1"; }
header(){ echo -e "\n${CYAN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"; echo -e "${GREEN}  $1${NC}"; echo -e "${CYAN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}\n"; }

install_skills() {
  local phase_name="$1"; shift
  local skills=("$@")
  local installed=0 skipped=0 missing=0

  for skill in "${skills[@]}"; do
    if [ -d "$REPO_DIR/$skill" ]; then
      if [ -d "$SKILLS_TARGET/$skill" ]; then
        warn "已存在，跳过: $skill"
        ((skipped++))
      else
        cp -r "$REPO_DIR/$skill" "$SKILLS_TARGET/$skill"
        ok "$skill"
        ((installed++))
      fi
    else
      warn "仓库中未找到: $skill (请检查技能名拼写)"
      ((missing++))
    fi
  done

  echo ""
  info "${phase_name} 完成 — 新装: ${installed}  跳过: ${skipped}  未找到: ${missing}"
}

# ============================================================================
#  前置检查
# ============================================================================
header "前置检查"

if [ ! -d "$REPO_DIR" ]; then
  info "克隆 OpenClaw-Medical-Skills 仓库 (仅技能目录，跳过大文件)..."
  git clone --depth=1 --no-checkout \
    https://github.com/FreedomIntelligence/OpenClaw-Medical-Skills.git
  cd OpenClaw-Medical-Skills
  git sparse-checkout init --cone
  git sparse-checkout set skills
  git checkout main
  cd ..
  ok "仓库就绪"
else
  ok "仓库已存在: $REPO_DIR"
fi

mkdir -p "$SKILLS_TARGET"
ok "技能目标目录: $SKILLS_TARGET"

# ============================================================================
#  第一阶段: 核心 CRO 技能包 (1-2 周见效)
#  目标: 药物发现 + 药效评价 + 病理分析 + 肿瘤研究 + 文献报告
# ============================================================================
header "第一阶段: 核心 CRO 技能包"

PHASE1_SKILLS=(
  # --- 药物发现与药效评价 (对标: 临床前CRO药效评价、药物筛选、适应症筛选) ---
  "agentd-drug-discovery"                   # 自主药物发现 Agent (靶点→先导→ADMET)
  "chemcrow-drug-discovery"                 # ChemCrow 药物发现工具包
  "tooluniverse-drug-research"              # 多数据库药物研究 (ChEMBL 生物活性)
  "tooluniverse-drug-repurposing"           # 药物重定位候选识别
  "tooluniverse-drug-drug-interaction"      # DDI 预测 (CYP450/转运体/多药分析)
  "tooluniverse-pharmacovigilance"          # FDA 不良事件报告分析 (PRR/ROR)
  "chembl-database"                         # ChEMBL 化合物查询 (IC50, Ki)

  # --- 病理学与医学影像 (对标: 病理实验平台 HE/IHC/IF/TUNEL) ---
  "histolab"                                # 数字病理 WSI 处理 (HE/IHC 切片提取)
  "pathml"                                  # 计算病理工具包 (WSI + 多参数成像)
  "pydicom"                                 # DICOM 医学影像文件处理

  # --- 肿瘤学研究 (对标: 皮下/转移/原位肿瘤动物模型, PDOX/PDXO) ---
  "autonomous-oncology-agent"               # 自主肿瘤学 Agent (文献+试验匹配+标志物)
  "precision-oncology-agent"                # 精准肿瘤学 (分子图谱→治疗建议)
  "cosmic-database"                         # COSMIC 癌症突变数据库

  # --- 科研文献与报告 (对标: 课题设计、实验报告、注册申报文件) ---
  "pubmed-search"                           # PubMed 全面检索
  "biomedical-search"                       # 综合生物医学搜索 (PubMed+预印本+FDA)
  "medical-research-toolkit"                # 14+ 生物医学数据库联合查询
  "clinical-reports"                        # ICH 标准临床前报告/诊断报告/CSR
  "scientific-writing"                      # 科学手稿两阶段写作
  "scientific-slides"                       # 研究演示幻灯片 (PPT/Beamer)

  # --- 类器官 & 精准医疗 (对标: 药敏筛选、毒性评价、个性化检测) ---
  "tooluniverse-precision-oncology"         # 精准肿瘤学治疗建议 (类器官药敏决策)
  "tooluniverse-clinical-trial-matching"    # 患者-试验匹配 (精准医疗转化)
)

install_skills "第一阶段" "${PHASE1_SKILLS[@]}"

# ============================================================================
#  第二阶段: 分子与组学能力增强 (2-4 周)
#  目标: 蛋白质分析 + 通路验证 + 细胞/流式 + 数据可视化
# ============================================================================
header "第二阶段: 分子与组学能力增强"

PHASE2_SKILLS=(
  # --- 蛋白质与分子分析 (对标: 蛋白实验室 WB/蛋白定量/相互作用) ---
  "alphafold-database"                      # AlphaFold 2亿+ 蛋白质结构预测
  "alphafold"                               # AlphaFold 结构预测工具
  "string-database"                         # STRING PPI 查询 (5900万蛋白质)
  "biopython"                               # 分子生物学 Python 工具包
  "rdkit"                                   # 化学信息学 (SMILES/描述符/指纹)

  # --- 基因与分子通路 (对标: 分子生物学平台 RT-qPCR/基因表达/通路验证) ---
  "bio-de-deseq2-basics"                    # DESeq2 差异表达分析
  "ensembl-database"                        # Ensembl 基因组数据库 (250+ 物种)
  "clinvar-database"                        # ClinVar 临床变异数据库
  "ncbi-gene-database"                      # NCBI Gene 查询 (RefSeqs/GO/表型)

  # --- 细胞生物学 & 流式 (对标: 细胞培养/增殖/凋亡/流式/外泌体) ---
  "bio-single-cell-clustering"              # Scanpy/Seurat 单细胞聚类
  "scanpy"                                  # scRNA-seq 全流程分析
  "bio-single-cell-cell-annotation"         # 单细胞类型注释
  "tcr-repertoire-analysis-agent"           # TCR 库分析 (免疫学研究)

  # --- 数据可视化 & 报告 (对标: 实验数据展示/学术汇报) ---
  "matplotlib"                              # 出版级 Python 绑图
  "seaborn"                                 # 统计可视化 (箱线图/热图)
  "plotly"                                  # 交互式可视化 (探索性分析)
  "bio-data-visualization-heatmaps-clustering"  # 层次聚类热图
  "bio-reporting-automated-qc-reports"      # 自动化 QC 报告
  "latex-posters"                           # LaTeX 学术海报
)

# install_skills "第二阶段" "${PHASE2_SKILLS[@]}"

# ============================================================================
#  第三阶段: 全平台智能化 & 新业务扩展 (1-3 月)
#  目标: 肿瘤深度 + 免疫治疗 + 基因组学 + 实验室自动化 + AI设计
# ============================================================================
header "第三阶段: 全平台智能化 & 新业务扩展"

PHASE3_SKILLS=(
  # --- 肿瘤学深度 (扩展肿瘤模型业务的AI能力) ---
  "tumor-clonal-evolution-agent"            # 肿瘤克隆进化建模
  "liquid-biopsy-analytics-agent"           # 液体活检分析 (ctDNA/MRD)
  "tooluniverse-variant-interpretation"     # ACMG 标准临床变异解读

  # --- 免疫学与细胞治疗 (扩展免疫药效评价) ---
  "cart-design-optimizer-agent"             # CAR-T 设计优化
  "immune-checkpoint-combination-agent"     # 免疫检查点组合策略预测

  # --- 基因组学工具链 (未来基因组学服务方向) ---
  "tooluniverse-gwas-trait-to-gene"         # GWAS 关联基因发现
  "tooluniverse-gwas-drug-discovery"        # GWAS→药物靶点转化
  "bio-variant-calling"                     # GATK 种系变异检测
  "bio-variant-annotation"                  # VCF 功能注释

  # --- 实验室自动化 & LIMS (数字化升级) ---
  "opentrons-integration"                   # Opentrons 液体处理机器人
  "benchling-integration"                   # Benchling ELN 集成
  "instrument-data-to-allotrope"            # 仪器数据→ASM JSON 标准化

  # --- AI 驱动蛋白质 / 抗体设计 (新业务拓展) ---
  "antibody-design-agent"                   # AI 抗体设计
  "protac-design-agent"                     # PROTAC 降解剂设计
  "aav-vector-design-agent"                 # AAV 载体设计 (基因治疗)
)

# install_skills "第三阶段" "${PHASE3_SKILLS[@]}"

# ============================================================================
#  收尾
# ============================================================================
header "部署完成"

TOTAL=$(find "$SKILLS_TARGET" -maxdepth 1 -mindepth 1 -type d | wc -l)
info "技能目录: $SKILLS_TARGET"
info "已安装技能总数: $TOTAL"
echo ""
echo -e "${GRAY}提示:${NC}"
echo -e "  • OpenClaw 用户: 下次会话自动生效，无需重启"
echo -e "  • NanoClaw 用户: 请运行 ${CYAN}./container/build.sh${NC} 重建容器"
echo -e "  • 查看已装技能: ${CYAN}ls $SKILLS_TARGET${NC}"
echo -e "  • 卸载单个技能: ${CYAN}rm -rf $SKILLS_TARGET/<skill-name>${NC}"
echo ""
ok "晶莱华科 AI Agent 技能部署完毕 🧬"