#!/bin/bash
# SandD Daemon Installation Script
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/InftyAI/SandD/main/hack/scripts/install.sh | bash
#   curl -fsSL https://raw.githubusercontent.com/InftyAI/SandD/main/hack/scripts/install.sh | bash -s -- --tunnel
#
# Or locally:
#   ./scripts/install.sh
#   ./scripts/install.sh --tunnel

set -e

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Configuration
INSTALL_TUNNEL=false
SANDD_VERSION="${SANDD_VERSION:-latest}"
INSTALL_DIR="/usr/local/bin"

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --tunnel)
            INSTALL_TUNNEL=true
            shift
            ;;
        --version)
            SANDD_VERSION="$2"
            shift 2
            ;;
        --help)
            echo "SandD Daemon Installation Script"
            echo ""
            echo "Usage:"
            echo "  $0 [options]"
            echo ""
            echo "Options:"
            echo "  --tunnel       Install with tunnel support (includes Tailscale)"
            echo "  --version VER  Install specific version (default: latest)"
            echo "  --help         Show this help message"
            echo ""
            echo "Examples:"
            echo "  $0                    # Direct mode only"
            echo "  $0 --tunnel           # With tunnel support"
            exit 0
            ;;
        *)
            echo -e "${RED}Unknown option: $1${NC}"
            echo "Use --help for usage information"
            exit 1
            ;;
    esac
done

# Helper functions
log_info() {
    echo -e "${GREEN}==>${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}Warning:${NC} $1"
}

log_error() {
    echo -e "${RED}Error:${NC} $1"
}

check_root() {
    if [[ $EUID -ne 0 ]]; then
        log_error "This script must be run as root (use sudo)"
        exit 1
    fi
}

detect_os() {
    if [[ -f /etc/os-release ]]; then
        . /etc/os-release
        OS=$ID
        OS_VERSION=$VERSION_ID
    elif [[ "$(uname)" == "Darwin" ]]; then
        OS="macos"
    else
        log_error "Unsupported operating system"
        exit 1
    fi

    ARCH=$(uname -m)
    case $ARCH in
        x86_64)
            ARCH="amd64"
            ;;
        aarch64|arm64)
            ARCH="arm64"
            ;;
        *)
            log_error "Unsupported architecture: $ARCH"
            exit 1
            ;;
    esac

    log_info "Detected: $OS ($ARCH)"
}

install_dependencies() {
    log_info "Installing dependencies..."

    case $OS in
        ubuntu|debian)
            apt-get update
            apt-get install -y curl ca-certificates
            ;;
        centos|rhel|rocky|fedora)
            yum install -y curl ca-certificates
            ;;
        macos)
            # Assume Homebrew is installed
            if ! command -v brew &> /dev/null; then
                log_warn "Homebrew not found. Install from https://brew.sh"
            fi
            ;;
        *)
            log_warn "Unknown OS, skipping dependency installation"
            ;;
    esac
}

install_sandd() {
    log_info "Installing SandD daemon..."

    # Check if Rust/Cargo is installed
    if command -v cargo &> /dev/null; then
        log_info "Installing via cargo..."
        cargo install sandd
    else
        # Download binary
        log_info "Downloading binary..."
        DOWNLOAD_URL="https://github.com/InftyAI/SandD/releases/download/${SANDD_VERSION}/sandd-${OS}-${ARCH}"

        curl -fsSL "$DOWNLOAD_URL" -o /tmp/sandd
        chmod +x /tmp/sandd
        mv /tmp/sandd "$INSTALL_DIR/sandd"

        log_info "Installed to $INSTALL_DIR/sandd"
    fi
}

install_tailscale() {
    log_info "Installing Tailscale..."

    case $OS in
        ubuntu|debian)
            curl -fsSL https://tailscale.com/install.sh | sh
            ;;
        centos|rhel|rocky|fedora)
            curl -fsSL https://tailscale.com/install.sh | sh
            ;;
        macos)
            if command -v brew &> /dev/null; then
                brew install tailscale
            else
                log_warn "Please install Tailscale from https://tailscale.com/download"
            fi
            ;;
        *)
            log_warn "Please install Tailscale manually from https://tailscale.com/download"
            ;;
    esac

    # Verify installation
    if command -v tailscale &> /dev/null; then
        log_info "Tailscale installed: $(tailscale version | head -1)"
    else
        log_error "Tailscale installation failed"
        exit 1
    fi
}

verify_installation() {
    log_info "Verifying installation..."

    if command -v sandd &> /dev/null; then
        log_info "✓ sandd installed at: $(command -v sandd)"
    else
        log_error "sandd installation verification failed"
        exit 1
    fi

    if [[ "$INSTALL_TUNNEL" == "true" ]]; then
        if command -v tailscale &> /dev/null; then
            log_info "✓ Tailscale installed at: $(command -v tailscale)"
        else
            log_error "Tailscale installation verification failed"
            exit 1
        fi
    fi
}

print_next_steps() {
    echo ""
    echo -e "${GREEN}========================================${NC}"
    echo -e "${GREEN}Installation Complete!${NC}"
    echo -e "${GREEN}========================================${NC}"
    echo ""

    if [[ "$INSTALL_TUNNEL" == "true" ]]; then
        echo "Tunnel mode installed."
        echo ""
        echo "To run the daemon:"
        echo ""
        echo "  sandd --server-url ws://10.200.0.1:8765/ws \\"
        echo "        --daemon-id worker-1 \\"
        echo "        --tunnel \\"
        echo "        --tunnel-authkey YOUR_KEY \\"
        echo "        --tunnel-server http://headscale:8080"
    else
        echo "Direct mode installed."
        echo ""
        echo "To run the daemon:"
        echo ""
        echo "  sandd --server-url ws://controller:8765/ws --daemon-id worker-1"
    fi

    echo ""
    echo "Documentation: https://github.com/InftyAI/SandD/tree/main/docs"
    echo ""
}

# Main installation flow
main() {
    echo ""
    echo "SandD Daemon Installer"
    echo "======================"
    echo ""

    if [[ "$INSTALL_TUNNEL" == "true" ]]; then
        log_info "Mode: Tunnel (with Tailscale)"
    else
        log_info "Mode: Direct"
    fi
    echo ""

    check_root
    detect_os
    install_dependencies
    # install_sandd

    if [[ "$INSTALL_TUNNEL" == "true" ]]; then
        install_tailscale
    fi

    verify_installation
    print_next_steps
}

# Run main
main
