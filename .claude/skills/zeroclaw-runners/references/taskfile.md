# Runner Taskfile Reference

Location: `~/actions-runner/Taskfile.yml`

## Variables

| Var | Default | Description |
|-----|---------|-------------|
| `RUNNER_COUNT` | 3 | Number of new runners to create |
| `TOKEN` | (required) | GitHub registration token |
| `REPO_URL` | zeroclaw-labs/zeroclaw | Target repository |
| `BASE_NAME` | mMacBook | Runner name prefix |
| `LABELS` | self-hosted,macOS,x64 | Initial labels |
| `TARBALL` | ~/actions-runner/actions-runner-osx-x64-2.332.0.tar.gz | Runner package |

## Full Taskfile

```yaml
version: "3"

vars:
  RUNNER_COUNT: "3"
  TOKEN: ""
  REPO_URL: "https://github.com/zeroclaw-labs/zeroclaw"
  BASE_NAME: "mMacBook"
  LABELS: "self-hosted,macOS,x64"
  TARBALL: "~/actions-runner/actions-runner-osx-x64-2.332.0.tar.gz"

tasks:
  run-multi:
    desc: Set up multiple GitHub Actions runners
    requires:
      vars: [TOKEN]
    cmds:
      - |
        for i in $(seq 2 $(({{.RUNNER_COUNT}} + 1))); do
          DIR=~/actions-runner-$i
          mkdir -p $DIR && cd $DIR
          tar xzf {{.TARBALL}}
          ./config.sh \
            --url {{.REPO_URL}} \
            --token {{.TOKEN}} \
            --name "{{.BASE_NAME}}-$i" \
            --labels "{{.LABELS}}" \
            --unattended
          ./svc.sh install
          ./svc.sh start
          echo "Runner {{.BASE_NAME}}-$i started"
        done

  status:
    desc: Check status of all runners
    cmds:
      - |
        for dir in ~/actions-runner-*/; do
          if [ -f "$dir/.service" ]; then
            echo "=== $(basename $dir) ==="
            cd "$dir" && ./svc.sh status 2>/dev/null || echo "Not installed as service"
          fi
        done

  stop-all:
    desc: Stop all runner services
    cmds:
      - |
        for dir in ~/actions-runner-*/; do
          if [ -f "$dir/.service" ]; then
            echo "Stopping $(basename $dir)..."
            cd "$dir" && ./svc.sh stop 2>/dev/null || true
          fi
        done

  uninstall-all:
    desc: Stop and uninstall all runner services
    cmds:
      - |
        for dir in ~/actions-runner-*/; do
          if [ -f "$dir/.service" ]; then
            echo "Uninstalling $(basename $dir)..."
            cd "$dir" && ./svc.sh stop 2>/dev/null || true
            cd "$dir" && ./svc.sh uninstall 2>/dev/null || true
          fi
        done

  start-all:
    desc: Start all runner services
    cmds:
      - |
        for dir in ~/actions-runner ~/actions-runner-*/; do
          if [ -f "$dir/.service" ]; then
            echo "Starting $(basename $dir)..."
            cd "$dir" && ./svc.sh start 2>/dev/null || true
          fi
        done

  logs:
    desc: View live logs from all runners
    cmds:
      - tail -f ~/Library/Logs/actions.runner.*/Runner_*.log

  remove:
    desc: Unregister a runner from GitHub
    requires:
      vars: [TOKEN, DIR]
    cmds:
      - cd {{.DIR}} && ./config.sh remove --token {{.TOKEN}}

  cpu-info:
    desc: Show CPU info for scaling decisions
    cmds:
      - |
        LOGICAL=$(sysctl -n hw.logicalcpu)
        echo "Logical CPUs: $LOGICAL"
        echo "Recommended runners: $((LOGICAL / 2))"

  new-token-url:
    desc: Open browser to get a new registration token
    cmds:
      - open "https://github.com/zeroclaw-labs/zeroclaw/settings/actions/runners/new?arch=x64&os=osx"
```

## Usage Examples

```bash
# Add 2 runners
task run-multi TOKEN=ABCD1234 RUNNER_COUNT=2

# Check all services
task status

# View logs
task logs

# Scale down
task stop-all
task uninstall-all
```
