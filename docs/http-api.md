# ESP32-C3 内置管理页面与 HTTP 调参

**当前运行策略：HTTP 启动后由 ESP32 独立运行，不需要网页保活。关闭页面、切后台或 Wi-Fi 断开不停止电机。`lease_ms=0`；`POST /api/keepalive` 仅返回状态。手动 stop、设定时长和设备侧故障/采样保护仍有效。下文旧测试记录中的 500ms 网页租期已取消；USB 连续命令的租期不变。**
新增 [旋钮手感模式及 API](haptic-modes.md)，入口 `/#knob`：自由、段落、两档、五档、回中及正反棘轮。使用角度反馈换相，先校准后启用，开环转动接口保留。


## 当前使用方式：直接访问 ESP32-C3

完整管理页面已编译进板子的 Flash，由 `GET /` 提供。打开 **http://192.168.0.102/** 即可调参；编码器读数页为 **http://192.168.0.102/#encoder**。电脑或手机需与板子连接同一局域网，不需要 Python 服务、电脑代理或互联网资源。DHCP 地址可能变化；烧录后重新联网可能需要数十秒。

保留电压 0.5–2V、频率 0–100Hz、正反转开关、短时/持续运行、停止按钮及 AS5600 实时读数。设备地址显示当前网页的地址，不能在此页面修改 Wi-Fi 或切换其他设备。操作记录只存在于当前浏览器页面。

内置页面直接发送 URL 表单给板端 API。`stop` 和 `keepalive` 请求体为空；不再访问本机专用 `/api/settings`。所有脚本和样式内联，页面以静态 Flash 数据分段发送，不整体复制进运行堆。修改页面需重新编译烧录，日常调整参数无需烧录。

打开页面、刷新和应用参数不会启动电机。启动后由板端独立运行，页面隐藏/关闭或网络中断均不停止；所有页面均可查看状态和手动停止。当前仍为开环控制，编码器只读显示。

页面源码：`src/bin/motor_http/panel.html`。`admin/server.py` 和 `admin/index.html` 保留为可选的旧版本机代理，日常无需启动；下方“本机 JSON API”仅适用于该代理，不是板端协议。

验证页面传输适配：`node --test admin/test_embedded_panel.js`，覆盖表单参数、0Hz/反转、空体停止/保活、故障回执及离开页面的停止请求。

ESP32-C3 当前地址为 `http://192.168.0.102/`，由现有 Wi-Fi 的 DHCP 分配，重启后可能变化。
可选旧版 Python 后台为 `http://127.0.0.1:8766/`，只接受本机访问。

## 可选：启动旧版本机代理

在项目根目录运行；只依赖 Python 标准库：

```sh
python3 admin/server.py --device http://192.168.0.102
```

网页中可修改设备地址，确认连通后保存到 `.local/motor-admin.json`。
后台启动、打开网页、应用参数均不会启动电机，必须单独点击启动。
默认选择 3 秒试运行；持续运行需明确选择。页面隐藏或关闭不影响运行。
停止本机后台用其终端中的 Ctrl-C；ESP32-C3 继续按原设置独立运行。

## 本机 JSON API

| 方法 / 路径 | 内容 |
|---|---|
| `GET /api/status` | 从板子读取运行、故障、参数和在线时间 |
| `POST /api/config` | `{"voltage":0.8,"frequency_hz":8}`，立即设置连续模式参数，不自动启动；可选 direction 为 forward/reverse |
| `POST /api/start` | `{"duration_ms":3000}`；0 为持续模式，最大 30000ms；省略时默认 3000ms |
| `POST /api/keepalive` | `{}`，为当前 HTTP 运行续期 |
| `POST /api/stop` | `{}`，关闭输出 |
| `GET /api/settings` | 读取当前板子地址 |
| `POST /api/settings` | `{"device_url":"http://192.168.0.102"}`，验证并保存；运行时不允许切换设备 |

```sh
curl --noproxy '*' http://127.0.0.1:8766/api/status
curl --noproxy '*' -X POST http://127.0.0.1:8766/api/config \
  -H 'Content-Type: application/json' \
  -d '{"voltage":0.8,"frequency_hz":8}'
curl --noproxy '*' -X POST http://127.0.0.1:8766/api/stop \
  -H 'Content-Type: application/json' -d '{}'
```

本机后台只转发请求，不会在后台替客户端永久发送保活。
程序通过 start 启动后，应每 150–200ms 调用 keepalive，并在退出时调用 stop。
单独发送一次 start 即可；板端按 duration_ms 计时，0 表示持续运行。
网页只负责发送操作和显示状态，运行不依赖后续网络请求。

## 直接调用板子

板子同样提供 `/api/status`、`/api/config`、`/api/start`、`/api/keepalive`、`/api/stop`。
直接 POST 给板子时使用 URL 表单；内置网页也使用相同格式。可选旧版 Python 代理负责转换它收到的 JSON：

```sh
curl --noproxy '*' http://192.168.0.102/api/status
curl --noproxy '*' -X POST http://192.168.0.102/api/config \
  -d 'voltage=0.8&frequency_hz=8'
curl --noproxy '*' -X POST http://192.168.0.102/api/stop
```

状态例子：

```json
{"running":false,"fault":false,"mode":"stopped","direction":"forward","voltage":1.0,"frequency_hz":8.0,"lease_ms":500,"uptime_ms":12345}
```

`voltage`、`frequency_hz`、`direction` 是连续模式目标值；`mode` 区分停止、连续和旧串口脉冲诊断。
它们不是实测电压、电流或机械转速，`fault:false` 也不代表温升安全。

## 参数与运行约束

- 默认 1V、8Hz。运行时可设幅值 0.5–2V、电角频率 0–100Hz；NaN、无穷、越界和重复参数被拒绝。
- 调频保留当前电角度，电角频率以最高 50Hz/s 平滑过渡；电压幅值平滑接近新目标。
- 参数保存在 RAM，重启恢复默认值。调整参数无需编译或烧录；修改固件功能或 Wi-Fi 编译配置仍需烧录。
- 上电默认停机。故障脚 LOW 时拒绝启动，运行故障、20ms 更新停顿都会关闭输出，HTTP 不使用网络保活。
- HTTP 运行不依赖网络或串口保活；重新联网不会改变运行状态。
- 请求大小、TCP 等待时间及每轮处理量有界，网络处理不会等待客户端补全请求。
- 无电流、温度或编码器反馈；1V 的长期及带载温升仍未验证，不自动恢复此前发烫的 2V 配置。

## 固件构建

```sh
cargo build --offline --locked --release --bin motor-driver
espflash flash --port /dev/cu.usbmodem21201 --non-interactive --skip-update-check \
  target/riscv32imc-unknown-none-elf/release/motor-driver
```

默认启用 `motor-http`，Wi-Fi 凭证来自被 Git 忽略的 `.local/wifi.env`（复制 `wifi.env.example` 后填写）。
构建环境变量 `MOTOR_WIFI_SSID`、`MOTOR_WIFI_PASSWORD` 可覆盖配置，旧 Wi-Fi 示例也共用此配置。未配置时使用空值供 CI 编译，无法实际联网。凭证不会通过接口返回；不要上传含凭证的固件产物。
无 Wi-Fi 的旧串口固件可用 `--no-default-features` 构建。
串口 `net` 可查询地址；运行时不要另开串口，以免 USB 打开操作复位主控。

## 文件与验证

- `admin/server.py`、`admin/index.html`：本机后台及控制页面。
- `src/bin/motor_http/`：板端 Wi-Fi、DHCP、TCP/HTTP 及独立页面。
- `motor-control/src/api.rs`：有界 HTTP 解析和参数校验。
- `motor-control/src/lib.rs`：运行时参数、相位保持、幅值平滑和停机保护。

```sh
python3 -m unittest discover -s admin -p 'test_*.py'
python3 -m unittest discover -s scripts -p 'test_*.py'
cargo clippy --offline --locked --release --bin motor-driver -- -D warnings
cd /tmp
cargo +stable test --offline --locked --manifest-path /Users/lishuang/workDir/esp32c3-proj/motor-control/Cargo.toml
```

本次验证：16 项 Rust 测试、7 项本机后台测试、4 项串口脚本测试通过，默认及无 Wi-Fi 模式 clippy 通过。
已烧录 Wi-Fi HTTP 版本并取得 DHCP 地址；本机 JSON 参数请求成功转发到板子，直接回读确认 0.75V/10Hz，随后恢复默认 1V/8Hz。
验证了越界拒绝、停机、无运行时保活拒绝、地址保存、两种页面加载，以及空闲后 12 次并发状态查询。
这些接口验证没有自动启动电机；之后网页已显示实际启停日志和运行回执，电机实际转速与温升仍需单独观察或测量。
本机后台运行在独立的 8766 端口，避免与其他项目占用的 8765 冲突；在启动终端按 Ctrl-C 可停止后台。

## 扩展范围与反转

按用户要求，幅值范围扩展为 **0.5–2V**，电角频率范围为 **0.5–100Hz**。
方向字段为 `"direction":"forward"`（正转）或 `"direction":"reverse"`（反转），省略时保留当前方向。
正反转是相对电机现有相序而言，不保证对应某一观察面的顺时针方向。

本机请求示例：

```sh
curl --noproxy '*' -X POST http://127.0.0.1:8766/api/config \
  -H 'Content-Type: application/json' \
  -d '{"voltage":0.5,"frequency_hz":5,"direction":"reverse"}'
```

直接调用板子则发送表单 `voltage=0.5&frequency_hz=5&direction=reverse`。
方向发生变化时，板子先停止运行并关闭 EN，清除原保活和时长状态，再保存新方向。
不会自动反向启动；等电机停稳后再次调用 start 或点击启动。
修改幅值或频率且方向不变时，可在运行中平滑更新，不重置电角度。

2V 此前曾造成明显发热，此次扩大可设置范围不代表验证了 2V/100Hz 的电流、温升或带载性能。
此版本通过 21 项 Rust 测试、10 项本机后台测试以及 release 构建和 clippy。

已烧录扩展范围与反转版本；通过本机 JSON 和板端表单验证了范围边界、无效方向拒绝及方向保留。
验证期间保持停机，结束恢复 0.5V / 5Hz / 正转；未自动进行 2V / 100Hz 转动测试，也未实测反转机械动作。


## 2026-10-18：主芯片发热后的功耗优化

用户确认仅 USB 供电时主芯片持续发烫；切换到关闭无线、80MHz/WFI 的诊断固件后报告“不烫了”。这说明原固件功耗很可能是主要因素，但未测温度或电流。

当前已恢复并烧录优化后的 `motor-driver`：

- CPU 从 160MHz 调整为 80MHz（当前 esp-radio 要求至少 80MHz）；APB/LEDC 的 PWM 配置仍为 20kHz。
- 停机时主任务每轮用 RTOS 定时等待 5ms，无待处理任务时进入 RTOS 的 WFI 空闲任务；不再全速轮询 USB/TCP。
- 运行时等待到下一个 500µs 控制时点，超过时点不额外等待；20ms 更新停顿保护保持不变。
- Wi-Fi 停机使用 Minimum modem-sleep；启动前切换为 None，完成切换后再开始计时，避免省电唤醒延迟影响运行保活。停机关闭 EN 后恢复省电。
- 500ms 保活、运行时长、故障停机、正反转及参数范围保持不变。上电保持停止，不自动恢复运行。

验证：release 构建、默认/无 Wi-Fi 模式 clippy、21 项 Rust 控制测试和 10 项本机后台测试通过。烧录并回读确认 HTTP 恢复连接。初次查询测试出现一次 502，后续连续 40 次状态查询全部成功，中位约 103ms、最大约 518ms；省电下请求延迟受路由器 DTIM 和无线链路影响。
当时 nFT 为 LOW，启动返回 409，输出保持关闭；未绕过故障检测，因此本次未验证活动输出的调度时序、带电机运行和温升。实际功耗/温升未测量，仍需用户观察。AS5600 尚未接入固件。


## AS5600 实时诊断

内置页面 `http://192.168.0.102/#encoder`，侧栏“磁编码器”菜单，直接读取板端 `/api/status` 的 `encoder` 字段（旧版本机代理也会原样转发）：

```json
{"encoder":{"online":true,"paused":false,"raw_angle":2048,"angle_valid":true,"angle_deg":180.0,"status":32,"magnet":"ok","age_ms":20,"error":""}}
```

`online` 表示最近一次 I²C 读取成功；`magnet` 为 `ok/missing/weak/strong/invalid/unknown`，不能用通信成功代替磁场就绪。
只有磁场正常且样本新鲜（运行 <25ms、停机 <250ms），`angle_valid` 才为 true，`angle_deg` 才返回角度；其余情况为 null。`raw_angle` 是原始寄存器值，磁场异常时不可信。
无应答或超时会清除旧读数并返回 `online:false` 与 `error:nack/timeout/i2c`；恢复后自动重试。当前版本运行期间继续采集，`paused:false`；后文保留了从旧版暂停行为迁移的记录。
停机采样频率约 10Hz；后台页面在可见且选择编码器页时约每 200ms 发起一次状态查询（实际刷新受请求耗时影响），设备控制页保持约 1s。无闭环控制、无自动电机动作、无 OTP 写入。


### 原始角度展示调整

用户反馈约 5mm 间距已有寄存器读数。为便于调试，后台现在始终将在线传感器的 `raw_angle × 360 / 4096` 显示为角度，不因 MD=0 或磁场告警隐藏读数；未满足硬件磁场条件时用黄色指针并标注“未验证 · 原始换算角度”。暂停/过期的数据显示缓存标记，I²C 断开则清空。
磁场状态文字区分“磁场偏弱 · 未达检测阈值”和通信离线。芯片状态位、API 的 `angle_valid`/`angle_deg` 语义不变，也未将未验证值接入电机闭环。该变化仅修改管理页面，不需烧录。
参考：ams AS5600 数据手册给出典型气隙 0.5–3mm，实际取决于磁铁，不是所有磁铁都必须小于 3mm 的硬限制。尚未实测用户当前 5mm 间距下的连续性、重复性或精度。


## 0Hz 设置

频率范围现为 **0–100Hz**，负值、NaN、无穷和大于 100 的值仍拒绝。示例：

```sh
curl --noproxy '*' -X POST http://127.0.0.1:8766/api/config \
  -H 'Content-Type: application/json' \
  -d '{"voltage":0.5,"frequency_hz":0}'
```

配置不会自动启动。启动 0Hz 后保持静止磁场，输出仍使能；运行中调到 0 时频率平滑降到零，不立即断电。彻底关闭驱动仍需 stop。保活、时长和故障保护不变。开环静止磁场可能产生定位手感，但不提供可调段距或单向棘轮控制；AS5600 仍未参与闭环。
本次 26 项 Rust 测试、12 项后台测试、默认/无 Wi-Fi clippy 与 release 构建通过。已烧录并验证 0Hz 请求与负值拒绝，结束保持停机；没有自动通电进行保持力或温升试验。


## 持续采集版本（替代运行暂停行为）

`encoder.paused` 保留兼容字段，当前固定 false；运行时不再停采集。新增 `sample_seq` 成功采样计数、`errors` 累计 I²C 错误、`target_hz`（运行 200、停机 10，目标而非实测速率）。后台显示计数，寄存器数值不变时也能确认持续采样。
`angle_valid` 还要求磁场正常且数据新鲜：运行采样年龄 <25ms、停机 <250ms。未验证时仍按原始值换算展示；闭环控制尚未启用。
状态顶层新增 `control_max_gap_us`，记录本次运行中观察到的最大控制更新间隔；每次显式启动重置。500µs 是调度目标，不是硬实时保证，超过 20ms 仍触发原有停机保护。
HTTP JSON 固定容量扩展到 768 字节，避免新增诊断字段截断状态。

采集线程通过 `esp-radio-rtos-driver` 接口运行在同一 RTOS 上；默认 motor-http 固件启用它，无 Wi-Fi 的旧 USB 固件仍仅提供电机控制。USB 新增只读 `encoder` 命令，报告成功计数、错误、采样年龄与剩余堆；`net` 等待时也报告链路状态，不返回 Wi-Fi 凭证。


持续采集验收：27 项 Rust 测试、12 项后台测试，默认/无 Wi-Fi clippy 与 release 构建通过。固件已烧录并恢复网络。
在 0.5V/1Hz 约 2 秒输出使能测试期间，成功采样计数从 340 增到 600，实测约 154.8Hz，I²C 错误计数未增加（0），最大样本年龄 18ms，最大控制更新间隔 4951µs。
500µs 为目标，实测存在 RTOS/无线调度抖动，不能宣称恒定 2kHz 或硬实时 FOC。保活中断后自动停机已实测；结束恢复 1.25V/1Hz/正转，保持停机。
实物旋转效果、精度和热稳态没有测量；读取仍为 magnet:missing，未验证角度不能接入闭环。


## 2026-10-18：完整管理页面迁入 ESP32-C3

`src/bin/motor_http/panel.html` 现包含完整控制台、正反转开关和编码器实时页，直接访问 `http://192.168.0.102/`。内置页使用板端 URL 表单协议，停止/保活为空请求体；不依赖本机 `/api/settings` 或 Python 服务。页面 23,773 字节，静态保存在 Flash；固件镜像 525,856 字节。

release 构建、clippy 和 4 项页面请求测试通过；已烧录并从板子完整读回 HTML，校验字节及哈希一致。实机验证了 0Hz/反转、100Hz、负频率拒绝、停止、无运行时保活拒绝及采样计数增长。全过程未启动输出，结束恢复升级前的 1V/8Hz/正转。

本机 8766 Python 进程已停止，端口不再监听，之后板端页面和状态接口仍可独立访问。当前故障脚 LOW，输出关闭；编码器在线、磁场未达检测阈值。浏览器自动化工具连续超时，因此未完成实际页面点击/截图验收，接口和页面资源已验证。


## 2026-10-21：成品 AS5600 状态读取复核

用户质疑成品编码器的磁场告警。新增独立诊断 `src/bin/encoder_diagnostic.rs`，关闭 Wi-Fi/RTOS/PWM，并保持 EN=0；用阻塞 I²C 分别读取 0x0B STATUS、0x0C RAW_ANGLE、0x1A AGC、0x1B MAGNITUDE，同时对照原来的 0x0B 三字节连读。地址参考 [ams 原厂读取示例](https://look.ams-osram.com/m/dea9085545cea895/original/AS5600_AB-POT_UG000378_1-00.pdf)。无配置或 OTP 写入。

实机 20 次独立读取全部成功，单读 STATUS 与连读首字节均为 0x33；AGC=128，MAGNITUDE=1720–1729。原始角度随转动变化（采样范围 60–4059）；分次读取存在时间差，不要求转动时两次角度完全一致。此次未发现状态字节读取错位的证据，也不能据此判定成品损坏或确认全圈精度。

管理页面修正：将“磁铁检测”和“芯片强度告警”分开表述，MD=1 的告警状态显示“已检测到”，角度保持实时展示，不再统一标为“未验证”。API 的 angle_valid 仍保留严格的新鲜度/无磁场告警语义，未启用闭环或屏蔽任何驱动保护。

5 项页面测试、独立诊断 clippy 和正式固件 release 构建通过。已恢复正式固件并回读确认新版页面由板子提供；恢复测试前的 1V/30Hz/反转配置，保持停机。浏览器操作工具超时，未完成截图验收。独立诊断日志仅保存在被忽略的 .local/encoder-audit.log。

## 手感弹跳修正后的编码器配置

当前旋钮模式会临时配置 AS5600 的低延迟滤波，已不再是全程只读驱动：CONF 使用读改写并读回验证，停止后恢复原配置，不执行 OTP/BURN。当前目标采集 1kHz、总线 400kHz，运行/停机调度优先级及手感力度限制详见 [haptic-modes.md](haptic-modes.md)。历史章节中的 100kHz、200Hz 和“仅只读”描述对应之前版本。`encoder.conf` 字段返回当前读回的完整 CONF 数值，配置失败时为 null，并使采样不可用。

## 网络任务隔离后的状态快照

TCP、DHCP/Wi-Fi 管理和状态 JSON 编码在独立 HTTP 任务中执行，主控制线程只交换有界信箱消息及不可变快照。API 路径及表单格式不变；后续已取消 HTTP 保活依赖。JSON 增加 `status_format_us` / `network_poll_us` / `control_work_us` 三个最大墙钟耗时字段，重置于新会话启动；网络两项耗时不再阻塞控制。`uptime_ms` 和 `encoder.age_ms` 在控制端捕获快照时计算，未计浏览器传输耗时。当前网络优先级 2、控制/采集 20；主线程和后台不持锁执行网络或格式化。总堆为 64KiB reclaimed + 64KiB 普通堆，实测空闲约 19KB。优先级 30 的历史自由模式加载测试最大控制间隔为 2.091ms；当前优先级 20 的已校准静止段落测试最大间隔为 5.389ms，不能混作同一负载结果。详见 [haptic-modes.md](haptic-modes.md)。


### 高频反馈诊断

`GET /api/haptic/trace` 返回最近 256 个控制更新的二进制只读记录，`application/octet-stream`，不启动或停止电机。停止后保留，下一次启用手感/校准重置。格式和实测时序见 [haptic-modes.md](haptic-modes.md) 的高速记录章节。内存不足返回 503。当前反馈/采集优先级恢复为 30，HTTP 不作为运行寿命条件。


高速记录格式新增 v2：头仍为 `HKTR` + u16 LE 版本 + u16 LE 样本数；版本 2 每点 20 字节，前 16 字节保持 v1 的字段顺序，再追加 u16 `calculation_us`（从控制入口到 PWM 已应用的墙钟时间，含抢占）、u16 保留字段 0。Python 解包格式 `<IHHhhhHHH`。v1 保存记录仍是 16 字节 `<IHHhhhH`。新记录时间戳位于应用 PWM 后、诊断转换前；不能将其与 v1 的应用前时间戳视作完全相同的测量边界。
