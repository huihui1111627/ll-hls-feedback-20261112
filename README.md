# LL-HLS 低延迟直播服务（Rust 自研）

不依赖 ffmpeg / 任何现成 mp4 库：TCP 接入裸 H.264（含 B 帧）+ AAC，服务端解析时间戳、手写 fMP4 封装、产出 LL-HLS（partial segment + blocking reload），1080p/480p 双码率。

## 快速开始

```bash
# 首次：生成测试素材（60s、带毫秒时间码、B 帧、480p 关键帧错开 0.7s）
./scripts/gen_assets.sh

# 起服务 + 两路推流（可选 DROP=12:4 模拟断流；端口被占用时用 HTTP_ADDR/INGEST_ADDR 换）
./scripts/run_demo.sh
# HTTP_ADDR=0.0.0.0:8090 INGEST_ADDR=0.0.0.0:9001 DROP=12:4 ./scripts/run_demo.sh

# 浏览器打开 http://127.0.0.1:8080/
```

浏览器页面用 hls.js 播放，左上角是烧录在画面里的毫秒时间码，页面卡片里有 PDT 估算的端到端延迟。

## 架构

```
推流端 ──TCP(自定义帧头)──> ingest ──> h264 解析 ──> segmenter ──> fmp4 封装
                                                    │
                                         内存滑动窗口(10×2s segment + parts)
                                                    │
                         trailer/http (axum) <──────┘
                         ├─ /live/master.m3u8
                         ├─ /live/{1080p|480p}/index.m3u8   (EXT-X-PART + blocking reload)
                         ├─ init.mp4 / seg/{msn}.m4s / part/{msn}/{idx}.m4s
                         └─ /  (测试页)
```

| 模块 | 内容 |
|---|---|
| `proto` | 推流协议：26 字节帧头（stream_id、flags、pts/dts 90kHz、长度） |
| `server/src/h264.rs` | Annex-B 分 NAL、SPS 解析（Exp-Golomb）、AVCC 转换 |
| `server/src/fmp4.rs` | 手写 ftyp/moov/moof/mdat、avcC、esds；trun v1 写 B 帧的 CTS offset |
| `server/src/segmenter.rs` | 每路码率状态机：切 part(333ms)/segment(2s)、滑动窗口、断流重连、时间戳 rebase |
| `server/src/playlist.rs` | master / media playlist 渲染，PROGRAM-DATE-TIME |
| `server/src/http.rs` | axum 路由、blocking reload（`_HLS_msn`/`_HLS_part` + watch 广播） |
| `pusher` | 测试推流工具：读 ES + ffprobe 时间戳，按真实节奏推，支持循环和模拟断流 |

## 关键设计

- **延迟**：part 333ms，`PART-HOLD-BACK=1.0s`，hls.js 配 `liveSyncDuration:1.2`。实测端到端 ~2.0–2.6s。
- **B 帧**：推流端按解码序推 pts/dts，服务端写 trun v1 的有符号 composition offset，tfdt 用 dts。
- **切码率不黑屏**：每个 segment 在 IDR 处切分且 `INDEPENDENT-SEGMENTS`；两路关键帧不对齐没关系，hls.js 按 PDT 对齐两条时间轴。两路 init 的 profile/level 一致。
- **音画同步**：每路 fMP4 内同时封装 AAC 音轨（音频 dts 1920 ticks/帧 @48kHz → 90kHz），由播放器保证音画对齐。
- **断流重连**：重连后丢弃非关键帧直到下一个 IDR（旧参考帧已失效），旧 segment 收尾，插 `EXT-X-DISCONTINUITY`；时间戳 rebase 保持单调。断流期间服务端判定"停滞"，blocking reload 立即返回当前列表，恢复后自动续播。
- **容量**：所有媒体在内存、`Arc<Bytes>` 共享零拷贝；500 并发实测 RSS ~90–120MB，CPU <15%。

## 验证

```bash
cargo test                     # SPS 解析 / AVCC / box 布局 / B 帧偏移 / PDT 格式
./scripts/smoke.sh 1080p       # 拉 init+segment+part 用 ffprobe/ffmpeg 校验
./scripts/smoke.sh 480p
VIEWERS=500 DURATION=90 node scripts/loadtest.mjs   # 500 路并发压测
```

实测数据（本机，500 路 480p，90s）：

```
requests 106371 / 1155 rps / 0 errors / 603.9 Mbps
playlist p50 223ms（含阻塞等待）  media p50 357ms（客户端排队，10 路时 34ms）
服务端 RSS 稳定 ~90MB
```

`scripts/gen_assets.sh` 生成素材时把 480p 的关键帧强制在 0.7/2.7/4.7s…，与 1080p 的 0/2/4s 错开，专门覆盖不对齐场景。

## 已知取舍 / 后续

- 断流时长超过播放缓冲（~1.5s）时必然有一次停顿，这是低延迟的固有取舍；缓冲加长可减少停顿但延迟变高。
- 推流协议是自定义帧头，生产上需要给推流端发协议文档（`proto/src/lib.rs` 顶部注释）。
- 内存在窗口内；窗口外 segment/part 返回 404（正常 live 行为）。长时间断流后旧内容不可回溯。
- 未做鉴权/HTTPS/多推流源热备/CDN 适配，这些属于部署层。
