#!/usr/bin/env bash
# setup-server.sh: turn a fresh Ubuntu 24.04 x86_64 box into a profiling workstation.
#
# Run as root (or with sudo) right after the first SSH login:
#   sudo bash setup-server.sh --user mctursh
#
# Options:
#   --user NAME        your working (non-root) user, created if missing   (required when run as root; default: $SUDO_USER)
#   --repo URL         git repo to clone into ~/profiling-lab              (default: https://github.com/mctursh/profiling-lab)
#   --isolate CPUS     reserve CPUs for pinning, e.g. 4-7 (adds isolcpus/nohz_full/rcu_nocbs; needs a reboot)
#   --no-harden        skip SSH hardening and the firewall
#
# Safe to run again: every step checks before it changes anything.

set -euo pipefail

# ---------------------------------------------------------------------------
# Args
# ---------------------------------------------------------------------------
USER_NAME="${SUDO_USER:-}"
[[ "$USER_NAME" == "root" ]] && USER_NAME=""
REPO_URL="https://github.com/mctursh/profiling-lab"
ISOLATE=""
HARDEN=1

while [[ $# -gt 0 ]]; do
  case "$1" in
    --user)      USER_NAME="$2"; shift 2 ;;
    --repo)      REPO_URL="$2";  shift 2 ;;
    --isolate)   ISOLATE="$2";   shift 2 ;;
    --no-harden) HARDEN=0;       shift ;;
    -h|--help)   sed -n '2,15p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 1 ;;
  esac
done

step() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }
warn() { printf '\033[33mwarning:\033[0m %s\n' "$*"; }

[[ $EUID -eq 0 ]] || { echo "run as root: sudo bash $0 $*" >&2; exit 1; }
[[ -n "$USER_NAME" ]] || { echo "pass your username: bash $0 --user <name>  (root login gets disabled, so this is the user you'll log in as)" >&2; exit 1; }
. /etc/os-release
[[ "$ID" == "ubuntu" ]] || warn "written for Ubuntu 24.04, found $PRETTY_NAME"
[[ "$(uname -m)" == "x86_64" ]] || warn "written for x86_64, found $(uname -m)"

export DEBIAN_FRONTEND=noninteractive
USER_HOME="/home/$USER_NAME"
as_user() { sudo -u "$USER_NAME" -H bash -lc "$1"; }

# ---------------------------------------------------------------------------
# 1. Packages
# ---------------------------------------------------------------------------
step "Installing packages"
apt-get update -q
apt-get install -y -q \
  build-essential pkg-config git curl ca-certificates unzip \
  clang llvm libclang-dev cmake make protobuf-compiler libprotobuf-dev \
  libssl-dev libudev-dev zlib1g-dev \
  htop sysstat numactl hwloc stress-ng trace-cmd tmux jq \
  ufw unattended-upgrades \
  linux-tools-common linux-tools-generic
# perf built for this exact kernel, if the provider's kernel has one
apt-get install -y -q "linux-tools-$(uname -r)" 2>/dev/null \
  || warn "no linux-tools for kernel $(uname -r); using the generic perf"

# ---------------------------------------------------------------------------
# 2. Working user
# ---------------------------------------------------------------------------
step "User: $USER_NAME"
if ! id "$USER_NAME" &>/dev/null; then
  adduser --disabled-password --gecos "" "$USER_NAME"
fi
usermod -aG sudo "$USER_NAME"
# passwordless sudo, since there is no password to type
echo "$USER_NAME ALL=(ALL) NOPASSWD:ALL" > "/etc/sudoers.d/90-$USER_NAME"
chmod 440 "/etc/sudoers.d/90-$USER_NAME"

# copy root's SSH keys to the user if the user has none yet
install -d -m 700 -o "$USER_NAME" -g "$USER_NAME" "$USER_HOME/.ssh"
if [[ ! -s "$USER_HOME/.ssh/authorized_keys" && -s /root/.ssh/authorized_keys ]]; then
  install -m 600 -o "$USER_NAME" -g "$USER_NAME" /root/.ssh/authorized_keys "$USER_HOME/.ssh/authorized_keys"
fi

# ---------------------------------------------------------------------------
# 3. Security: key-only SSH, firewall, automatic security updates
# ---------------------------------------------------------------------------
if [[ $HARDEN -eq 1 ]]; then
  step "Hardening SSH and enabling the firewall"
  if [[ -s "$USER_HOME/.ssh/authorized_keys" ]]; then
    # "00-" so it wins over cloud-init's 50-cloud-init.conf (sshd keeps the first value it reads)
    cat > /etc/ssh/sshd_config.d/00-hardening.conf <<'EOF'
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin no
EOF
    sshd -t && systemctl reload ssh
  else
    warn "$USER_NAME has no SSH key; leaving SSH login settings unchanged so you aren't locked out"
  fi

  ufw allow OpenSSH >/dev/null
  ufw --force enable >/dev/null
  dpkg-reconfigure -f noninteractive unattended-upgrades
fi

# ---------------------------------------------------------------------------
# 4. Profiling permissions (persist across reboots)
# ---------------------------------------------------------------------------
step "Allowing perf to see everything"
cat > /etc/sysctl.d/99-profiling.conf <<'EOF'
kernel.perf_event_paranoid = -1
kernel.kptr_restrict = 0
EOF
sysctl -q --system

# make `perf` work even when the wrapper can't find a kernel-matched build
if ! perf --version &>/dev/null; then
  PERF_BIN="$(ls -d /usr/lib/linux-tools/*/perf 2>/dev/null | sort -V | tail -1)"
  [[ -n "$PERF_BIN" ]] && ln -sf "$PERF_BIN" /usr/local/bin/perf
fi

# ---------------------------------------------------------------------------
# 5. Rust + profiling tools (as the user)
# ---------------------------------------------------------------------------
step "Rust toolchain"
if ! as_user 'command -v cargo' &>/dev/null; then
  as_user 'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rustup-init.sh \
           && sh /tmp/rustup-init.sh -y --profile default && rm /tmp/rustup-init.sh'
fi
as_user 'rustup update stable'

step "samply and flamegraph"
as_user 'command -v samply  >/dev/null || cargo install --locked samply'
as_user 'command -v flamegraph >/dev/null || cargo install --locked flamegraph'

# ---------------------------------------------------------------------------
# 6. The lab
# ---------------------------------------------------------------------------
step "profiling-lab"
if [[ -d "$USER_HOME/profiling-lab/.git" ]]; then
  as_user "cd ~/profiling-lab && git pull --ff-only"
else
  as_user "git clone '$REPO_URL' ~/profiling-lab"
fi
as_user 'cd ~/profiling-lab && cargo build --release'

# ---------------------------------------------------------------------------
# 7. Optional: reserve CPUs for pinned threads
# ---------------------------------------------------------------------------
if [[ -n "$ISOLATE" ]]; then
  step "Reserving CPUs $ISOLATE (isolcpus, nohz_full, rcu_nocbs)"
  ARGS="isolcpus=$ISOLATE nohz_full=$ISOLATE rcu_nocbs=$ISOLATE"
  # Provider images override /etc/default/grub with files in grub.d (Cherry's
  # 99-cherry.cfg blanks the cmdline), so add our own file that sorts last.
  sed -i -E 's/ ?(isolcpus|nohz_full|rcu_nocbs)=[^" ]*//g' /etc/default/grub
  echo "GRUB_CMDLINE_LINUX=\"\$GRUB_CMDLINE_LINUX $ARGS\"" > /etc/default/grub.d/99-zz-isolate.cfg
  update-grub
  NEED_REBOOT=1
fi

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
step "Done"
echo "CPU:      $(lscpu | sed -n 's/^Model name: *//p')"
echo "CPUs:     $(nproc)   (cores: $(lscpu -p=CORE | grep -v '^#' | sort -u | wc -l))"
echo "Memory:   $(free -h | awk '/^Mem:/ {print $2}')"
echo "Kernel:   $(uname -r)"
echo "perf:     $(perf --version 2>/dev/null || echo 'not working')"
echo "Isolated: $(cat /sys/devices/system/cpu/isolated 2>/dev/null || true)"
echo
echo "Next:"
echo "  1. In a NEW terminal, check you can log in:  ssh $USER_NAME@<server-ip>"
echo "     (keep this session open until that works)"
echo "  2. cd ~/profiling-lab && ./target/release/profiling-lab --seconds 10 --rate 0 --queue 8 --batch 128 --workers 8"
[[ "${NEED_REBOOT:-0}" -eq 1 ]] && echo "  3. Reboot to apply the CPU isolation:  sudo reboot"
exit 0