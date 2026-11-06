# Secure Mobile Print/Scan Server Architecture (ARM Laptop Deployment)

This document provides the security architecture, threat model, firewall configuration, and automated tunneling instructions for deploying the **Canon PIXMA G2010 Rust Suite** on a mobile Linux laptop (such as an ARM64 laptop running Arch Linux ARM / Asahi Linux) serving as a dedicated print and scan gateway for remote desktop workstations.

---

## 1. Threat Model & Security Environment

When an ARM laptop acts as a print server while traveling through **public Wi-Fi networks** (cafes, coworking spaces, airports, hotels):

| Vector | Risk | Mitigation |
| :--- | :--- | :--- |
| **Untrusted Local Network** | Malicious actors on the same public Wi-Fi scanning for open ports. | All PIXMA services bind strictly to `127.0.0.1` (loopback) and `/run/pixma/pixmad.sock`. Zero ports open to LAN. |
| **Unauthorized Printing / Ink Exhaustion** | Anyone on the LAN submitting bulk or abusive print jobs, exhausting ink and waste absorber capacity (`5B00`). | CUPS / IPP is not exposed via mDNS/Avahi or bound to `0.0.0.0`. Remote jobs require SSH or WireGuard authentication. |
| **Scanner Snooping** | Rogue clients accessing the flatbed scanner over eSCL HTTP and capturing sensitive documents on the platen. | eSCL HTTP server is bound to `127.0.0.1:8080`. Encrypted transport only. |
| **Host Compromise via SSH** | Compromised Wi-Fi network attempting brute-force SSH attacks on the laptop. | Password authentication disabled (`PasswordAuthentication no`), Ed25519 keys only, and restricted tunnel-only user accounts. |

---

## 2. Architecture Overview

```
                      COWORKING / PUBLIC WI-FI (Untrusted)
                      +-----------------------------------+
                      |      Dropped by nftables / ufw     |
                      +-----------------------------------+

[Desktop Workstation]                                       [Mobile ARM Laptop (Server)]
+--------------------+                                       +----------------------------+
| CUPS Client        |                                       | Local Loopback Only        |
| (ipp://127.0.0.1:   |                                       | - pixma-printer-app:8631   |
|        8631/print) |   Encrypted SSH Tunnel / WireGuard    | - pixma-escl-server:8080   |
|                    | ====================================> | - pixmad (/run/pixma.sock) |
| SANE Client        |   (Mutual Auth, No Remote Shell)      |             |              |
| (http://127.0.0.1: |                                       |             v              |
|        8080/eSCL)  |                                       | Canon PIXMA G2010 (USB)    |
+--------------------+                                       +----------------------------+
```

---

## 3. Server Configuration (ARM Laptop)

### Step 1: Ensure Local-Only Binding

The provided systemd services (`pixma-printer-app.service` and `pixma-escl.service`) bind exclusively to loopback by default:
- `pixma-printer-app --bind 127.0.0.1:8631`
- `pixma-escl-server --bind 127.0.0.1:8080`
- `pixmad --socket /run/pixma/pixmad.sock`

Verify that no service listens on `0.0.0.0` or public interfaces:
```bash
ss -tulpn | grep -E '8631|8080'
# Expected:
# tcp LISTEN 0 128 127.0.0.1:8631 0.0.0.0:*
# tcp LISTEN 0 128 127.0.0.1:8080 0.0.0.0:*
```

### Step 2: Firewall Lockdown (nftables / UFW)

If using `ufw`:
```bash
sudo ufw default deny incoming
sudo ufw default allow outgoing
# Allow SSH ONLY if needed, preferably restricted to key auth
sudo ufw allow 22/tcp
sudo ufw enable
```

If using `nftables` (`/etc/nftables.conf`):
```nftables
table inet filter {
    chain input {
        type filter hook input priority filter; policy drop;

        # Allow loopback traffic
        iif "lo" accept

        # Allow established and related connections
        ct state established,related accept

        # Allow SSH with rate limiting
        tcp dport 22 ct state new limit rate 5/minute accept

        # Reject all other inbound packets
        counter reject with icmpx type admin-prohibited
    }
}
```

### Step 3: Hardened, Tunnel-Only SSH Account

For maximum security on mobile laptops, create a dedicated user that cannot spawn an interactive shell:
```bash
sudo useradd -r -s /usr/bin/nologin -d /var/empty pixmatunnel
```

In the workstation's authorized keys file on the laptop (`~pixmatunnel/.ssh/authorized_keys` or user `~/.ssh/authorized_keys`):
```text
command="/bin/false",no-agent-forwarding,no-pty,no-user-rc,no-X11-forwarding,permitopen="127.0.0.1:8631",permitopen="127.0.0.1:8080" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI... workstation_pixma
```
> [!IMPORTANT]
> The `permitopen` directives strictly restrict the SSH connection to forwarding ports `8631` and `8080`. Even if someone captures the private key, they cannot execute shell commands or access any other network services on the laptop.

---

## 4. Connection Topology: Direct vs. Reverse Tunnel

Depending on whether the laptop has a reachable IP address:

### Topology A: Direct Tunnel (Workstation -> Laptop)
Used when both machines are on the same trusted home/office Wi-Fi, or connected via a private mesh VPN (WireGuard or Tailscale).

**Manual Command:**
```bash
ssh -N -T \
  -L 8631:127.0.0.1:8631 \
  -L 8080:127.0.0.1:8080 \
  -o ServerAliveInterval=15 \
  -o ServerAliveCountMax=3 \
  user@laptop-ip
```

### Topology B: Reverse Tunnel (Laptop -> Workstation)
Used when the laptop travels to coworking spaces or cafes behind NAT/firewalls and cannot accept inbound connections. The laptop initiates an outbound connection back to the workstation (or a private VPS):

**Laptop Command:**
```bash
ssh -N -T \
  -R 8631:127.0.0.1:8631 \
  -R 8080:127.0.0.1:8080 \
  -o ServerAliveInterval=15 \
  -o ServerAliveCountMax=3 \
  -o ExitOnForwardFailure=yes \
  user@workstation-host
```

---

## 5. Automated Systemd User Services

To ensure connections remain active transparently in the background with automatic reconnection upon sleep/wake or Wi-Fi changes:

### On the Workstation (Direct Tunnel Service):
Install `assets/systemd/pixma-ssh-tunnel.service` to `~/.config/systemd/user/`:
```bash
mkdir -p ~/.config/systemd/user/
cp assets/systemd/pixma-ssh-tunnel.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now pixma-ssh-tunnel.service
```

### On the Laptop (Reverse Tunnel Service):
Install `assets/systemd/pixma-reverse-tunnel.service` to `~/.config/systemd/user/`:
```bash
mkdir -p ~/.config/systemd/user/
cp assets/systemd/pixma-reverse-tunnel.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now pixma-reverse-tunnel.service
```

---

## 6. Client Configuration (Desktop Workstation)

Once the tunnel is active, the workstation accesses the printer and scanner via local loopback ports.

### 1. CUPS Printing Configuration (Driverless IPP Everywhere)
```bash
# Add the remote printer as a local driverless IPP printer
lpadmin -p Canon_G2010_Remote -E -v ipp://127.0.0.1:8631/ipp/print -m everywhere
```
Or check printer capabilities directly:
```bash
ipptool -v ipp://127.0.0.1:8631/ipp/print get-printer-attributes.test
```

### 2. SANE / AirScan Scanning Configuration
Edit `/etc/sane.d/escl.conf` (or `/etc/sane.d/airscan.conf`):
```ini
[Canon PIXMA G2010 Remote]
ip_address = 127.0.0.1
port = 8080
protocol = http
```

Test scan over tunnel:
```bash
scanimage -d "escl:http://127.0.0.1:8080" --format=png > /tmp/remote_scan.png
```

### 3. PIXMA CLI Remote Query
You can forward status queries using `curl`:
```bash
curl http://127.0.0.1:8631/status
```
Or forward the Unix socket over SSH:
```bash
ssh -nNT -L /tmp/pixmad_remote.sock:/run/pixma/pixmad.sock user@laptop
pixma-cli --socket /tmp/pixmad_remote.sock status
```

---

## 7. WireGuard Alternative (Recommended for Zero-Maintenance Mesh)

For an even cleaner setup without managing SSH sessions:
1. Setup a point-to-point WireGuard link between workstation (`10.42.0.1`) and laptop (`10.42.0.2`).
2. Configure `pixma-printer-app --bind 10.42.0.2:8631` and `pixma-escl-server --bind 10.42.0.2:8080`.
3. Firewall on laptop permits traffic on ports 8631 and 8080 **only** from interface `wg0`.
4. Workstation prints directly to `ipp://10.42.0.2:8631/ipp/print` with end-to-end ChaCha20-Poly1305 encryption and seamless roaming.
