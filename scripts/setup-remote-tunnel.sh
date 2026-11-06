#!/usr/bin/env bash
# ==============================================================================
# Setup / Test Helper for Secure Remote Print & Scan Server Tunnel
# ==============================================================================
set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0m'

IPP_PORT="${IPP_PORT:-8631}"
ESCL_PORT="${ESCL_PORT:-8080}"

echo -e "${BLUE}=== Canon PIXMA G2010 Secure Remote Tunnel Helper ===${NC}\n"

usage() {
    echo "Usage: $0 [test-local | test-remote <host> | install-client-unit <host> | install-server-unit <host>]"
    echo ""
    echo "Environment Variables (optional):"
    echo "  IPP_PORT    Local/remote IPP port (default: 8631)"
    echo "  ESCL_PORT   Local/remote eSCL AirScan port (default: 8080)"
    echo ""
    echo "Commands:"
    echo "  test-local             Verify local IPP ($IPP_PORT) and eSCL ($ESCL_PORT) ports"
    echo "  test-remote <host>     Test SSH connectivity and forwarding to <host>"
    echo "  install-client-unit    Install and enable systemd user tunnel on Workstation"
    echo "  install-server-unit    Install and enable systemd user reverse tunnel on Laptop"
    exit 1
}

CMD="${1:-}"

case "$CMD" in
    test-local)
        echo "Checking local loopback services (IPP: $IPP_PORT, eSCL: $ESCL_PORT)..."
        if curl -s -m 2 "http://127.0.0.1:${IPP_PORT}/status" >/dev/null 2>&1; then
            echo -e "  [${GREEN}OK${NC}] IPP / Printer Application responding on 127.0.0.1:${IPP_PORT}"
        else
            echo -e "  [${YELLOW}WARN${NC}] IPP Printer Application not responding on 127.0.0.1:${IPP_PORT}"
        fi

        if curl -s -m 2 "http://127.0.0.1:${ESCL_PORT}/eSCL/ScannerCapabilities" >/dev/null 2>&1; then
            echo -e "  [${GREEN}OK${NC}] eSCL AirScan Server responding on 127.0.0.1:${ESCL_PORT}"
        else
            echo -e "  [${YELLOW}WARN${NC}] eSCL AirScan Server not responding on 127.0.0.1:${ESCL_PORT}"
        fi
        ;;

    test-remote)
        REMOTE_HOST="${2:-}"
        if [ -z "$REMOTE_HOST" ]; then
            echo -e "${RED}Error: Remote host is required.${NC}"
            usage
        fi
        echo "Testing SSH connection to $REMOTE_HOST..."
        if ssh -o BatchMode=yes -o ConnectTimeout=5 "$REMOTE_HOST" true 2>/dev/null; then
            echo -e "  [${GREEN}OK${NC}] SSH authentication to $REMOTE_HOST succeeded."
        else
            echo -e "  [${RED}FAIL${NC}] Could not authenticate via SSH to $REMOTE_HOST."
            exit 1
        fi
        ;;

    install-client-unit)
        REMOTE_HOST="${2:-}"
        if [ -z "$REMOTE_HOST" ]; then
            echo -e "${RED}Error: Remote laptop host is required.${NC}"
            usage
        fi
        USER_UNIT_DIR="${HOME}/.config/systemd/user"
        mkdir -p "$USER_UNIT_DIR"
        sed -e "s/%u@laptop/$REMOTE_HOST/" \
            -e "s/8631:127.0.0.1:8631/${IPP_PORT}:127.0.0.1:${IPP_PORT}/" \
            -e "s/8080:127.0.0.1:8080/${ESCL_PORT}:127.0.0.1:${ESCL_PORT}/" \
            assets/systemd/pixma-ssh-tunnel.service > "$USER_UNIT_DIR/pixma-ssh-tunnel.service"
        systemctl --user daemon-reload
        systemctl --user enable --now pixma-ssh-tunnel.service
        echo -e "${GREEN}Installed and activated systemd user service 'pixma-ssh-tunnel.service' for $REMOTE_HOST!${NC}"
        ;;

    install-server-unit)
        REMOTE_HOST="${2:-}"
        if [ -z "$REMOTE_HOST" ]; then
            echo -e "${RED}Error: Workstation host is required.${NC}"
            usage
        fi
        USER_UNIT_DIR="${HOME}/.config/systemd/user"
        mkdir -p "$USER_UNIT_DIR"
        sed -e "s/%u@workstation/$REMOTE_HOST/" \
            -e "s/8631:127.0.0.1:8631/${IPP_PORT}:127.0.0.1:${IPP_PORT}/" \
            -e "s/8080:127.0.0.1:8080/${ESCL_PORT}:127.0.0.1:${ESCL_PORT}/" \
            assets/systemd/pixma-reverse-tunnel.service > "$USER_UNIT_DIR/pixma-reverse-tunnel.service"
        systemctl --user daemon-reload
        systemctl --user enable --now pixma-reverse-tunnel.service
        echo -e "${GREEN}Installed and activated reverse tunnel service 'pixma-reverse-tunnel.service' targeting $REMOTE_HOST!${NC}"
        ;;

    *)
        usage
        ;;
esac
