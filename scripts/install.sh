#!/usr/bin/env bash
set -euo pipefail

# Interactive Modular Installer for pixma-g2010-rs on Arch Linux
# Supports whiptail / dialog TUI fallback to CLI prompts.

RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0m'

echo -e "${BLUE}=== Canon PIXMA G2010 Rust Suite Modular Installer (2026) ===${NC}"

# Component selections default
INSTALL_CORE=true
INSTALL_CUPS=true
INSTALL_SANE=true
INSTALL_ESCL=true
INSTALL_PRINTER_APP=false
INSTALL_CLI=true
INSTALL_GUI=true
DRY_RUN=false

# CLI argument parsing for non-interactive/automated setups
while [[ $# -gt 0 ]]; do
    case "$1" in
        --all)
            INSTALL_CORE=true
            INSTALL_CUPS=true
            INSTALL_SANE=true
            INSTALL_ESCL=true
            INSTALL_PRINTER_APP=true
            INSTALL_CLI=true
            INSTALL_GUI=true
            shift
            ;;
        --core-only)
            INSTALL_CORE=true
            INSTALL_CUPS=false
            INSTALL_SANE=false
            INSTALL_ESCL=false
            INSTALL_PRINTER_APP=false
            INSTALL_CLI=true
            INSTALL_GUI=false
            shift
            ;;
        --no-gui)
            INSTALL_GUI=false
            shift
            ;;
        --dry-run)
            DRY_RUN=true
            shift
            ;;
        --help|-h)
            echo "Usage: $0 [OPTIONS]"
            echo "Options:"
            echo "  --all            Install all components including printer-app"
            echo "  --core-only      Install only udev rules, pixmad, and pixma-cli"
            echo "  --no-gui         Install CLI, CUPS, and SANE without GTK4 GUI"
            echo "  --dry-run        Print actions without installing files"
            echo "  --help, -h       Show this help message"
            exit 0
            ;;
        *)
            shift
            ;;
    esac
done

# Check for root / sudo if not a dry-run
SUDO=""
if [ "$DRY_RUN" = false ] && [ "$EUID" -ne 0 ]; then
    if command -v sudo >/dev/null 2>&1; then
        SUDO="sudo"
    else
        echo -e "${RED}Error: This script must be run as root or with sudo installed.${NC}"
        exit 1
    fi
fi

run_cmd() {
    if [ "$DRY_RUN" = true ]; then
        echo -e "  ${YELLOW}[DRY-RUN]${NC} $*"
    else
        "$@"
    fi
}

# If running interactively on a terminal with dialog/whiptail and no override flags passed
if [ -t 0 ] && [ "$DRY_RUN" = false ] && { command -v whiptail >/dev/null 2>&1 || command -v dialog >/dev/null 2>&1; }; then
    DIALOG_CMD="whiptail"
    if ! command -v whiptail >/dev/null 2>&1; then
        DIALOG_CMD="dialog"
    fi

    CHOICES=$($DIALOG_CMD --title "PIXMA G2010 Rust Suite Component Selection" \
        --checklist "Select the components to build and install:" 18 70 7 \
        "CORE" "Udev rules & pixmad multiplexer daemon (Required)" ON \
        "CUPS" "CUPS 2.x printing filters, backend & PPD" ON \
        "SANE" "SANE scanner backend (libsane-pixma_g2010.so)" ON \
        "ESCL" "Driverless eSCL AirScan HTTP server" ON \
        "IPP_APP" "Driverless IPP Everywhere Printer Application (CUPS 3.x)" OFF \
        "CLI" "Headless maintenance & status CLI (pixma-cli)" ON \
        "GUI" "PIXMA G2010 Studio (Relm4 / Libadwaita desktop app)" ON \
        3>&1 1>&2 2>&3 || true)

    if [ -n "$CHOICES" ]; then
        INSTALL_CORE=false
        INSTALL_CUPS=false
        INSTALL_SANE=false
        INSTALL_ESCL=false
        INSTALL_PRINTER_APP=false
        INSTALL_CLI=false
        INSTALL_GUI=false

        for choice in $CHOICES; do
            case "$choice" in
                \"CORE\"|CORE) INSTALL_CORE=true ;;
                \"CUPS\"|CUPS) INSTALL_CUPS=true ;;
                \"SANE\"|SANE) INSTALL_SANE=true ;;
                \"ESCL\"|ESCL) INSTALL_ESCL=true ;;
                \"IPP_APP\"|IPP_APP) INSTALL_PRINTER_APP=true ;;
                \"CLI\"|CLI) INSTALL_CLI=true ;;
                \"GUI\"|GUI) INSTALL_GUI=true ;;
            esac
        done
    fi
fi

echo -e "\n${YELLOW}Building selected components via Cargo...${NC}"
BUILD_CRATES=()
if [ "$INSTALL_CORE" = true ]; then BUILD_CRATES+=("-p" "pixma-daemon"); fi
if [ "$INSTALL_CUPS" = true ]; then BUILD_CRATES+=("-p" "pixma-cups"); fi
if [ "$INSTALL_SANE" = true ]; then BUILD_CRATES+=("-p" "pixma-sane"); fi
if [ "$INSTALL_ESCL" = true ]; then BUILD_CRATES+=("-p" "pixma-escl"); fi
if [ "$INSTALL_PRINTER_APP" = true ]; then BUILD_CRATES+=("-p" "pixma-printer-app"); fi
if [ "$INSTALL_CLI" = true ]; then BUILD_CRATES+=("-p" "pixma-cli"); fi
if [ "$INSTALL_GUI" = true ]; then BUILD_CRATES+=("-p" "pixma-gui"); fi

if [ ${#BUILD_CRATES[@]} -gt 0 ]; then
    cargo build --release "${BUILD_CRATES[@]}"
else
    echo "No crates selected to build."
    exit 0
fi

echo -e "\n${YELLOW}Installing components...${NC}"

if [ "$INSTALL_CORE" = true ]; then
    echo "Installing core daemon and udev rules..."
    run_cmd $SUDO install -Dm755 target/release/pixmad /usr/bin/pixmad
    run_cmd $SUDO install -Dm644 assets/udev/70-pixma-g2010.rules /usr/lib/udev/rules.d/70-pixma-g2010.rules
    run_cmd $SUDO install -Dm644 assets/systemd/pixmad.service /usr/lib/systemd/system/pixmad.service
    run_cmd $SUDO udevadm control --reload-rules
    run_cmd $SUDO udevadm trigger
    run_cmd $SUDO systemctl daemon-reload
    run_cmd $SUDO systemctl enable --now pixmad.service || true
fi

if [ "$INSTALL_CUPS" = true ]; then
    echo "Installing CUPS filters and PPD..."
    run_cmd $SUDO install -Dm755 target/release/rastertopixma-g2010 /usr/lib/cups/filter/rastertopixma-g2010
    run_cmd $SUDO install -Dm755 target/release/cmdtopixma-g2010 /usr/lib/cups/filter/cmdtopixma-g2010
    run_cmd $SUDO install -Dm755 target/release/cnusb-g2010 /usr/lib/cups/backend/cnusb-g2010
    run_cmd $SUDO install -Dm644 assets/ppd/pixma-g2010.ppd /usr/share/cups/model/pixma-g2010.ppd
    if [ "$DRY_RUN" = true ] || systemctl is-active cups >/dev/null 2>&1; then
        run_cmd $SUDO systemctl restart cups
    fi
fi

if [ "$INSTALL_SANE" = true ]; then
    echo "Installing SANE driver..."
    run_cmd $SUDO install -Dm755 target/release/libsane_pixma_g2010.so /usr/lib/sane/libsane-pixma_g2010.so.1.0.0
    run_cmd $SUDO ln -sf libsane-pixma_g2010.so.1.0.0 /usr/lib/sane/libsane-pixma_g2010.so.1
    run_cmd $SUDO ln -sf libsane-pixma_g2010.so.1 /usr/lib/sane/libsane-pixma_g2010.so
    run_cmd $SUDO mkdir -p /etc/sane.d/dll.d
    if [ "$DRY_RUN" = true ]; then
        echo -e "  ${YELLOW}[DRY-RUN]${NC} echo \"pixma_g2010\" > /etc/sane.d/dll.d/pixma_g2010"
    else
        echo "pixma_g2010" | $SUDO tee /etc/sane.d/dll.d/pixma_g2010 >/dev/null
    fi
fi

if [ "$INSTALL_ESCL" = true ]; then
    echo "Installing eSCL AirScan server..."
    run_cmd $SUDO install -Dm755 target/release/pixma-escl-server /usr/bin/pixma-escl-server
    run_cmd $SUDO install -Dm644 assets/systemd/pixma-escl.service /usr/lib/systemd/system/pixma-escl.service
    run_cmd $SUDO systemctl daemon-reload
fi

if [ "$INSTALL_PRINTER_APP" = true ]; then
    echo "Installing IPP Everywhere Printer Application..."
    run_cmd $SUDO install -Dm755 target/release/pixma-printer-app /usr/bin/pixma-printer-app
    run_cmd $SUDO install -Dm644 assets/systemd/pixma-printer-app.service /usr/lib/systemd/system/pixma-printer-app.service
    run_cmd $SUDO systemctl daemon-reload
fi

if [ "$INSTALL_CLI" = true ]; then
    echo "Installing pixma-cli and shell completions..."
    run_cmd $SUDO install -Dm755 target/release/pixma-cli /usr/bin/pixma-cli
    run_cmd $SUDO install -Dm644 assets/completions/pixma-cli.bash /usr/share/bash-completion/completions/pixma-cli
    run_cmd $SUDO install -Dm644 assets/completions/_pixma-cli /usr/share/zsh/site-functions/_pixma-cli
    run_cmd $SUDO install -Dm644 assets/completions/pixma-cli.fish /usr/share/fish/vendor_completions.d/pixma-cli.fish
fi

if [ "$INSTALL_GUI" = true ]; then
    echo "Installing PIXMA G2010 Studio GUI..."
    run_cmd $SUDO install -Dm755 target/release/pixma-studio /usr/bin/pixma-studio
    run_cmd $SUDO install -Dm644 assets/desktop/org.pixma.G2010Studio.desktop /usr/share/applications/org.pixma.G2010Studio.desktop
    run_cmd $SUDO install -Dm644 assets/desktop/org.pixma.G2010Studio.svg /usr/share/icons/hicolor/scalable/apps/org.pixma.G2010Studio.svg
fi

echo -e "\n${GREEN}=== Installation Complete! ===${NC}"
echo -e "You can now run:"
if [ "$INSTALL_CLI" = true ]; then echo -e "  • ${BLUE}pixma-cli status${NC} (Check ink levels and device state)"; fi
if [ "$INSTALL_GUI" = true ]; then echo -e "  • ${BLUE}pixma-studio${NC} (Launch Desktop Studio)"; fi
if [ "$INSTALL_CUPS" = true ]; then echo -e "  • Configure printer in CUPS web interface at ${BLUE}http://localhost:631${NC}"; fi
if [ "$INSTALL_SANE" = true ]; then echo -e "  • Scan with ${BLUE}simple-scan${NC} or ${BLUE}scanimage -L${NC}"; fi
