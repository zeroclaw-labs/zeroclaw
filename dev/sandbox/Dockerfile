FROM ubuntu:22.04

# Prevent interactive prompts during package installation
ENV DEBIAN_FRONTEND=noninteractive

# Install common development tools and runtimes
# - Node.js: Install v20 (LTS) from NodeSource
# - Core: curl, git, vim, build-essential (gcc, make)
# - Python: python3, pip
# - Network: ping, dnsutils
RUN apt-get update && apt-get install -y curl && \
    curl -fsSL https://deb.nodesource.com/setup_20.x | bash - && \
    apt-get install -y \
    nodejs \
    wget git vim nano unzip zip \
    build-essential \
    python3 python3-pip \
    sudo \
    iputils-ping dnsutils net-tools \
    && rm -rf /var/lib/apt/lists/* \
    && node --version && npm --version

# Create a non-root user 'developer' with UID 1000
# Grant passwordless sudo to simulate a local dev environment (using safe sudoers.d)
RUN useradd -m -s /bin/bash -u 1000 developer && \
    echo "developer ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/developer && \
    chmod 0440 /etc/sudoers.d/developer

# Set up the workspace
USER developer
WORKDIR /home/developer/workspace

# Default command
CMD ["/bin/bash"]
