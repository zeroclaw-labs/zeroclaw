# ZeroClaw 安全多租户部署方案

该目录包含一个用于在 Ubuntu 22.04+ 服务器上自动化部署一个安全、健壮、多租户 ZeroClaw 环境的 Shell 脚本。

## 特性

- **多租户隔离**: 为每个用户创建独立的 ZeroClaw 实例、工作区和配置。
- **安全第一**:
    - **Nginx 反向代理**: 所有访问均通过 Nginx，并启用 HTTP Basic Auth 进行密码保护。
    - **自动 HTTPS**: 使用 Let's Encrypt (certbot) 自动为所有用户子域名配置 SSL 证书，并强制 HTTPS。
    - **强化的防火墙**: 仅开放必要的端口 (SSH, HTTP/S)，内部服务端口不暴露于公网。
- **生产级进程管理**:
    - **Systemd 集成**: 每个实例都作为一个 systemd 服务运行，实现进程守护、崩溃后自动重启和开机自启。
    - **中央化日志**: 所有实例日志均由 `journald` 统一管理，便于查询和追溯。
- **强大的管理工具**:
    - `zeroclaw-ctl`: 一个简单易用的命令行工具，用于批量或单独管理所有实例 (启动、停止、查看状态、获取配对码、查看日志等)。
- **系统稳定性**:
    - **自动 Swap**: 脚本会自动创建 Swap 交换文件，防止服务器因内存不足而终止服务。

## 前置条件

在运行脚本之前，你必须准备好以下几项：

1.  **一台服务器**: 推荐配置为 4 核 CPU, 8GB RAM, 75G 存储，并安装了 Ubuntu 22.04 LTS。
2.  **一个域名**: 你需要拥有一个域名 (例如 `yourdomain.com`) 用于为用户分配子域名。
3.  **一个邮箱地址**: 用于注册 Let's Encrypt SSL 证书。
4.  **DNS 配置**: 在你的域名提供商处，提前或在部署后立即将 `agent1.yourdomain.com` 到 `agent20.yourdomain.com` 的 **A 记录**全部指向你服务器的公网 IP 地址。
5.  **API Keys**: 准备好你需要提供给用户的模型服务 API Key (例如 Google Gemini API Key)。

## 如何使用

1.  **克隆你的仓库**:
    ```bash
    git clone https://github.com/myhkstar/zeroclaw.git
    cd zeroclaw
    ```

2.  **配置脚本**:
    使用你喜欢的编辑器 (如 `nano` 或 `vim`) 打开 `scripts/deploy-multitenant.sh` 文件。
    ```bash
    nano scripts/deploy-multitenant.sh
    ```
    在文件顶部，**必须修改**以下两个变量：
    ```bash
    DOMAIN="yourdomain.com"
    CERTBOT_EMAIL="your-email@yourdomain.com"
    ```
    你也可以根据需要调整 `USER_COUNT` 和 `SWAP_SIZE`。

3.  **运行部署脚本**:
    赋予脚本执行权限并运行它。建议在 `screen` 或 `tmux` 会话中运行，以防 SSH 连接中断。
    ```bash
    chmod +x scripts/deploy-multitenant.sh
    sudo ./scripts/deploy-multitenant.sh
    ```
    脚本会自动完成所有系统配置、软件安装、实例创建和安全加固。过程可能需要 5-10 分钟。

## 部署后操作

脚本执行成功后，请按照以下步骤完成最后的设置：

1.  **启动服务**:
    ```bash
    # 将所有服务设置为开机自启
    sudo zeroclaw-ctl enable
    # 立即启动所有服务
    sudo zeroclaw-ctl start
    ```

2.  **分发凭据并配置 API Key**:
    -   初始的 Web 登录密码保存在 `/opt/zeroclaw/nginx/initial_credentials.txt`。请将每个用户的密码告知他们。
    -   通知每个用户使用 SSH 或其他方式登录服务器，并编辑他们自己的环境文件，例如 `user-001` 需要编辑 `/opt/zeroclaw/instances/user-001/.env`，在其中填入他们的 `GEMINI_API_KEY`。
    -   用户填完 Key 后，需要重启他们的实例才能生效：`sudo zeroclaw-ctl restart 1`。

3.  **获取配对码**:
    运行以下命令，获取所有用户的客户端配对码，并分发给他们。
    ```bash
    sudo zeroclaw-ctl pairing
    ```

4.  **[重要] 删除初始密码文件**:
    在确认所有用户都已收到他们的初始密码后，**立即删除**包含明文密码的文件！
    ```bash
    sudo rm /opt/zeroclaw/nginx/initial_credentials.txt
    ```

## 使用 `zeroclaw-ctl` 进行管理

`zeroclaw-ctl` 是你管理整个平台的主要工具。

-   **查看所有实例状态**:
    ```bash
    sudo zeroclaw-ctl status
    ```
-   **启动/停止/重启所有实例**:
    ```bash
    sudo zeroclaw-ctl start
    sudo zeroclaw-ctl stop
    sudo zeroclaw-ctl restart
    ```
-   **管理单个实例 (例如 user-005)**:
    ```bash
    sudo zeroclaw-ctl start 5
    sudo zeroclaw-ctl stop 5
    sudo zeroclaw-ctl restart 5
    ```
-   **查看单个实例的实时日志**:
    ```bash
    sudo zeroclaw-ctl logs 5
    ```
-   **重置用户的 Web 密码**:
    ```bash
    sudo zeroclaw-ctl password 5
    ```
