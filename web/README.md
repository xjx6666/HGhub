# HGhub 网页版（iPhone Safari 用）

桌面/手机 App 的数据、解密、视频流转发都在 Rust 层，纯前端部署是空壳。
这里是正确做法：复用 `src-tauri/src/` 的核心模块（`net`/`session`/`lines`/
`logging`/`network_policy` 原样引用，`media`/`playback_log` 因含 Tauri 类型而以
`web/src/media_web.rs`、`web/src/playback_log_web.rs` 两份网页版本地副本承载，
逻辑与上游一致、仅把 Tauri 传输类型换成普通结构体；Tauri 2 即使关掉默认特性
也会拖入 muda/GTK，不适合做服务端依赖），由 axum 提供 HTTP 服务；前端只改
`src/api.ts` 一个收口（`invoke` → `fetch`，`hg://` → `/hg` 代理）。

## 结构

- `web/src/main.rs`：路由、密码门、静态前端、媒体代理。
- `web/src/commands.rs`：36 个桌面命令的纯异步镜像（与 `src-tauri/src/api.rs` 同逻辑）。
- `web/src/media_web.rs`、`web/src/playback_log_web.rs`：上游同名模块的网页版本地副本（仅换掉 Tauri 类型/属性，逻辑一致；上游改动后重新复制并重做替换）。
- `web/login.html`：独立登录页（未登录时唯一能看到的页面）。
- `web/hghub-web.service`：systemd `--user` 服务，只绑 `127.0.0.1:8900`。
- `web/deploy.sh`：服务器上一键部署（拉代码 → 构建前后端 → 装服务 → 本地验通）。
- 前端开关：`VITE_HG_WEB=1 pnpm build` 打网页版；不带该变量就是原样桌面版。

## 密码门

- 启动必需 `HGHUB_WEB_PASSWORD_HASH`（密码的 sha256 hex，见 `deploy.sh` 生成，
  存在 `~/.config/hghub-web.env`，600 权限）。没设这个值服务拒绝启动。
- 未登录时：`/` 跳登录页，`/api/*` 与 `/hg/*` 返回 401，什么数据都看不到。
- 登录后 cookie 30 天有效。上游账号登录（App 内的登录/注册）与这道门是两回事。

## 本地开发/验证

```bash
pnpm install --frozen-lockfile
VITE_HG_WEB=1 pnpm build          # 产物在 dist/
export HGHUB_WEB_PASSWORD_HASH="$(printf '%s' '临时密码' | sha256sum | cut -d' ' -f1)"
cargo run --release --manifest-path web/Cargo.toml   # 127.0.0.1:8900
```

## 上线顺序（别反）

1. 服务器上跑 `bash web/deploy.sh`，先只绑本机，在服务器本地验通。
2. 再去云控制台 VCN 安全列表放行 `8900/TCP`。
3. 外网验真（缺一不可）：
   - 未登录访问首页被挡回登录页；
   - 登录后首页 feed 是真实数据；
   - 任选一部剧，详情能打开、HLS 清单和视频分片经代理返回 200、封面能加载；
   - iPhone Safari 打开可用，可添加到主屏幕。

## 注意

- 仅限个人非商业自用（PolyForm Noncommercial），与三端安装包同一红线。
- 改密码：改 `~/.config/hghub-web.env` 里的 HASH 后 `systemctl --user restart hghub-web`。
- 日志与会话存在 `~/.local/share/hghub-web/`（`session.json`、`debug.log` 等）。
