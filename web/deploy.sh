#!/usr/bin/env bash
# HGhub 网页版部署脚本（在服务器上以普通用户跑，如 ocarun）。
# 做完的事：拉 fork 分支 → 构建前端（VITE_HG_WEB=1）→ 构建 hghub-web →
# 装 systemd --user 服务 → 只绑 127.0.0.1:8900 本地验通。
# 开端口（VCN 安全列表放行 8900）是下一步，另行在控制台操作，本脚本不碰。
set -euo pipefail

# 安装前缀：默认 $HOME；在 InstaBox 上用 HGHUB_PREFIX=/data（/data 为持久盘，$HOME 是否持久未核实）。
PREFIX="${HGHUB_PREFIX:-$HOME}"

REPO_DIR="$PREFIX/HGhub"
BRANCH="web/hghub-web-20261117"
FORK="https://github.com/xjx6666/HGhub.git"
BIN_DIR="$PREFIX/.local/bin"
ENV_FILE="$PREFIX/.config/hghub-web.env"

echo "== 1/6 代码 =="
if [ -d "$REPO_DIR/.git" ]; then
  git -C "$REPO_DIR" fetch origin
  git -C "$REPO_DIR" checkout "$BRANCH"
  git -C "$REPO_DIR" pull --ff-only origin "$BRANCH"
else
  git clone --branch "$BRANCH" "$FORK" "$REPO_DIR"
fi

echo "== 2/6 Node（前端构建用） =="
if ! command -v node >/dev/null 2>&1; then
  ARCH="$(uname -m)"
  case "$ARCH" in
    aarch64) NODE_ARCH="arm64" ;;
    x86_64)  NODE_ARCH="x64" ;;
    *) echo "未知架构 $ARCH"; exit 1 ;;
  esac
  mkdir -p "$HOME/.local/node"
  curl -fsSL "https://nodejs.org/dist/v22.21.1/node-v22.21.1-linux-$NODE_ARCH.tar.xz" \
    | tar -xJ -C "$HOME/.local/node" --strip-components=1
  export PATH="$HOME/.local/node/bin:$PATH"
fi
export PATH="$HOME/.local/node/bin:$PREFIX/.local/bin:$HOME/.local/bin:$PATH"
node --version
if ! command -v pnpm >/dev/null 2>&1; then
  corepack enable 2>/dev/null || npm install -g pnpm@10
  corepack prepare pnpm@10 --activate 2>/dev/null || true
fi
pnpm --version

echo "== 3/6 前端构建（网页版） =="
cd "$REPO_DIR"
pnpm install --frozen-lockfile
VITE_HG_WEB=1 pnpm build
test -f dist/index.html

echo "== 4/6 Rust（后端构建用） =="
if ! command -v cargo >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain stable
fi
export PATH="$HOME/.cargo/bin:$PATH"
cargo --version

echo "== 5/6 后端构建并安装 =="
cargo build --release --manifest-path web/Cargo.toml
mkdir -p "$BIN_DIR"
cp web/target/release/hghub-web "$BIN_DIR/hghub-web"
chmod +x "$BIN_DIR/hghub-web"

echo "== 6/6 密码门 + 服务 =="
mkdir -p "$(dirname "$ENV_FILE")"
if ! grep -q "^HGHUB_WEB_PASSWORD_HASH=" "$ENV_FILE" 2>/dev/null; then
  PW="$(openssl rand -hex 16)"
  HASH="$(printf '%s' "$PW" | sha256sum | cut -d' ' -f1)"
  printf 'HGHUB_WEB_PASSWORD_HASH=%s\n' "$HASH" > "$ENV_FILE"
  chmod 600 "$ENV_FILE"
  echo ">>> 网页版密码（只显示这一次，立刻离线保存好）：$PW"
else
  chmod 600 "$ENV_FILE"
  echo ">>> 沿用已有的网页版密码（未改动）。"
fi

mkdir -p "$HOME/.config/systemd/user"
cp web/hghub-web.service "$HOME/.config/systemd/user/hghub-web.service"
# 前缀不是 $HOME 时，把 unit 里的 %h 换成实际前缀（WorkingDirectory/EnvironmentFile/ExecStart/数据与前端目录）
if [ "$PREFIX" != "$HOME" ]; then
  sed -i "s|%h|$PREFIX|g" "$HOME/.config/systemd/user/hghub-web.service"
fi
systemctl --user daemon-reload
systemctl --user enable hghub-web.service
systemctl --user restart hghub-web.service
loginctl enable-linger "$USER" 2>/dev/null \
  || echo "!! enable-linger 未成功（可能要 sudo）：重启后服务可能不自启，请手动处理。"

sleep 3
systemctl --user status hghub-web.service --no-pager | head -15
echo "== 本地验通（只绑 127.0.0.1） =="
curl -s -o /dev/null -w "登录页: %{http_code}\n" http://127.0.0.1:8900/login
curl -s -o /dev/null -w "未登录首页: %{http_code}（应为 307 跳登录页）\n" http://127.0.0.1:8900/
echo "本地通过后再去云控制台 VCN 安全列表放行 8900/TCP，然后按 web/README.md 的外网验真清单逐项核对。"
