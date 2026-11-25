# RePal — Clean-Room Rust & WebAssembly RPG Engine (1995 Isometric JRPG Reimplementation)

> **Legal Notice & Clean-Room Disclaimer / 法律声明与免责说明**
>
> **RePal** is an independent, non-commercial, open-source **clean-room engine reimplementation** written from scratch in Rust and JavaScript/WebAssembly, licensed under the [MIT License](./LICENSE).
>
> - **No Copyrighted Game Assets Included**: This repository contains **engine source code and tooling only**. It does **not** include, distribute, or link to any copyrighted original game binaries, maps, sprites, portraits, music, sound effects, scripts, or save files.
> - **Bring Your Own Legally Owned Assets**: To run a game, users must supply their own legally obtained copy of the original 1995 DOS game files (unpacked locally via `tools/build_assets.py` and passed with `--assets <DIR>`) or an authorized local/encrypted `.rpk` asset pack (`--packs-dir <DIR>` / `REPAL_PACK_KEY` / `<user_dir>/pack.key`).
> - **Trademarks**: *Chinese Paladin / The Legend of Sword and Fairy* (《仙剑奇侠传》) and all related characters, stories, artwork, and trademarks are the property of their respective copyright holders (Softstar Entertainment Inc. / 大宇资讯). This project is not affiliated with, endorsed by, or sponsored by any copyright holder.
>
> 本仓库为基于 [MIT 协议](./LICENSE) 开源的**纯引擎净室重写项目**，**不包含任何受版权保护的原版游戏程序、美术、音频、剧本或存档资产**。运行游戏需要用户自行提供合法取得的正版 1995 DOS 原版数据解包目录（`--assets <DIR>`）或受授权的加密 `.rpk` 资产包与密钥。

---

## 核心特性

1. **纯静态 Rust + WebAssembly 引擎 + 原生跨平台桌面端**：基于共享 `pal-runtime`（`pal-core` + `pal-render` + `pal-ui`）同时编译为 WebAssembly（[`site/`](./site/)，零后端、纯静态可部署 GitHub Pages）与统一原生可执行文件 `repal`（Windows / macOS / Linux）。
2. **确定性游戏核心 + 1995 经典 DOS UI**：
   - 全部游戏逻辑——剧本虚拟机（165 种指令）、战斗、菜单、存档与唯一权威状态——均运行在确定性 Rust 核心 `pal_core::game` 中，固定 20 ms 逻辑帧推进，不依赖平台时钟或非确定性随机数；
   - 探索世界、战斗演出（含仙术特效与跳字）、过场动画与 1995 原版风 UI 均由 `pal-render` + `pal-ui` 软渲染呈现。
   - **按需流式 LRU 资产缓存（256 MB 上限）与按关卡分片 `.rpk` 资产包（`RPK1` / `repal-packs/1`）**：支持按章节/地图/过场懒加载、SHA-256 校验、多镜像自动回退、可选每条 AES-256-GCM 加密与无损密钥轮换。
3. **双画质 / 双音质 `[F2]` 实时无缝切换**：
   - **画面**：支持 4× HD 分块流式地图与高分辨率精灵/立绘，游戏中随时按 **`[F2]`** 瞬间切换 `320×200` 1995 原生像素画质与 4× HD 画质；
   - **音质**：内置双模实时音频合成器——高清模式采用**古筝/竹笛/二胡物理谐波建模 + 立体声大厅混响**，按 **`[F2]`** 可在不中断节拍的情况下实时切换为 **1995 OPL2 声卡 FM 芯片音色**。
4. **可读 YAML 剧本与零编译 Mod 架构**：支持简体中文 / 繁體中文 / English 实时切换，通过本地覆盖目录（`--assets` 叠加 `--packs-dir` 或 YAML Mod）零编译修改剧情与支线。
5. **现代交互与确定性回放**：A\* 鼠标点击寻路、HTML5 手柄与录像影院、多槽位存档、分段式确定性 **`.repal` 录像**（章节缩略图跳转、一键接管、`debug_mode` 水印、4× HD AI 思考字幕轨与 `--replay-export-srt` 导出、`F9` 诊断上报）。
6. **统一原生二进制 `repal`（GUI + 无头文本模式 + 实时 AI 旁观）**：支持原生桌面 GUI、无窗口终端/CI 文本模式（`repal --text [--obs fair] [--json]`）与实时画面 + TCP/stdio 联动（`repal --listen 127.0.0.1:9999`，LAN 绑定强制 `auth <token>` 认证）。

---

## 技术文档

- [**`docs/ARCHITECTURE.md`**](./docs/ARCHITECTURE.md) — 共享 `pal-runtime` / 全 WASM / 统一原生 `repal` 架构、Track-B 分块流式内存预算与 `RePal_Assets` 解耦方案。
- [**`docs/USER_MANUAL.md`**](./docs/USER_MANUAL.md) — 原生 `repal` 桌面端使用手册（资产自动下载、热键、录像影院与 AI 旁观模式）。
- [**`docs/TEXT_MODE_SPEC.md`**](./docs/TEXT_MODE_SPEC.md) — 统一 Text Mode 协议规范（`GUI` / `Text` / `GUI+Text` × `stdio` / `socket` × `plain` / `--json`）。
- [**`docs/HD_REMASTER_METHODOLOGY.md`**](./docs/HD_REMASTER_METHODOLOGY.md) — Track-B 离线 AI 4× 高清重制方法论、无缝拼缝/跨场景色调校准与 5 项硬性验收门禁规范。
- [**`AGENTS.md`**](./AGENTS.md) — 面向 AI Agent 与贡献者的项目核心约束与开发指令。

---

## 本地快速运行（模拟 GitHub Pages 静态托管）

无需安装 Node.js，在仓库根目录使用 Python 内置静态文件服务器即可启动：

```bash
python3 -m http.server 8080
```

然后在浏览器访问：
- **http://localhost:8080/site/**（支持通过 `?assets=<CDN_URL>` 覆盖静态资产根目录）

### 快捷键说明（对齐 1995 原版 / SDLPAL）
- `方向键` / `小键盘 8 2 4 6` / **鼠标点击地面**：移动与自动寻路
- `Space` / `Enter` / `Z` / `左 Ctrl` / **点击 NPC 或宝箱**：对话、调查、确认、推进台词
- `Esc` / `Alt` / `Insert` / `小键盘 0` / `X` / **鼠标右键**：打开/关闭【状态 / 仙术 / 物品 / 系统】菜单与返回上一级
- `PgUp` / `PgDn` / `Home` / `End`（小键盘 `9/3/7/1`）：物品/仙术列表翻页与首尾跳转
- 战斗快捷键：`R` 重复、`A` 围攻、`D` 防御、`E` 使用道具、`W` 投掷、`Q` 逃跑、`F` 强行普攻、`S` 查看状态
- `F2`：**实时切换「4× 高清重制 + 古风混响」⇄「1995 原版像素 + OPL2 芯片音色」**
- `F7`：切换 4× HD **AI 思考字幕栏**显示/隐藏（复古模式不显示）
- `F8`（桌面端）：打开本地 `.repal` / `.json` 录像影院（影院中 `Tab` 开关章节目录）
- `F9`：一键诊断报障（网页端复制「现场快照 + 确定性录像」；桌面端保存 `.repal` + `.png` 并通过 `gh` 自动提交 Issue）
- `F10` / `F11`（桌面端）：保存 4× HD 截图 / 切换全屏（或 `Alt+Enter`）

---

## 部署到 GitHub Pages & CI

- **自动部署**：仓库已内置 [`.github/workflows/deploy-pages.yml`](./.github/workflows/deploy-pages.yml) 与 `site/.nojekyll`。在仓库 `Settings -> Pages -> Build and deployment` 将 Source 设为 **GitHub Actions** 后即可直接通过 `https://<你的用户名>.github.io/<仓库名>/` 游玩。
- **自动校验**：[`.github/workflows/ci.yml`](./.github/workflows/ci.yml) 在每次 push / PR 上运行全量 Rust/WASM、Node 与 Python 测试套件。

---

## 编译 WASM 与运行全量验证套件

```bash
# 1. Rust：游戏核心（剧本汇编、原生剧情通关、第一章对拍基线、NPC/战斗/物品回归、51 份原版存档）、
#    渲染/UI 单元测试与 pal-headless 视觉 E2E，然后编译 pal.wasm
cargo test --workspace
make build-wasm

# 2. Node：在无头浏览器环境里启动真实站点（标题 → 开场剧情 → 走路 → 逐帧回放 → 存读档 → 战斗）
#    以及宿主侧回归（音频、YAML 解析器、坐标换算、剪贴板）
node tools/test_rust_core_host.mjs
node tools/tests/test_host.mjs
node tools/tests/test_headless_harness.mjs

# 3. 运行全量数据完整性、空间分页遮挡与 GitHub Pages 静态规范测试
python3 -m pytest tools/tests/ -v

# （可选）从 DoSPal.zip 重新解包生成基础静态资产
cargo build -p pal-cli --release
python3 tools/build_assets.py
```
