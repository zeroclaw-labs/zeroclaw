FROM ubuntu:22.04

# Prevent interactive prompts during package installation
ENV DEBIAN_FRONTEND=noninteractive

# Install common development tools and runtimes
# - Core: curl, git, vim, build-essential (gcc, make)
# - Python: python3, pip
# - Node: nodejs, npm
# - Network: ping, dnsutils
RUN apt-get update && apt-get install -y \
    curl wget git vim nano unzip zip \
    build-essential \
    python3 python3-pip \
    nodejs npm \
    sudo \
    iputils-ping dnsutils net-tools \
    && rm -rf /var/lib/apt/lists/*

# Create a non-root user 'developer' with UID 1000
# Grant passwordless sudo to simulate a local dev environment
RUN useradd -m -s /bin/bash -u 1000 developer && \
    echo "developer ALL=(ALL) NOPASSWD:ALL" >> /etc/sudoers

# Set up the workspace
USER developer
WORKDIR /home/developer/workspace

# Default command
CMD ["/bin/bash"]
