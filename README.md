# smarkknob_v1

HTTP 管理页面只负责调参，启动后由 ESP32 独立运行；网页关闭、切后台或断网不会停机。板端仍响应手动停止、设定时长及硬件/编码器异常。旧 USB 连续命令仍需保活。

ESP32-C3 + DRV8313 SimpleFOCMini + 2804 电机 + AS5600 磁编码器智能旋钮原型。当前提供开环转动、动态调参、内置管理页面，以及基于 AS5600 角度反馈的电压手感控制（无电流反馈）。

新增 [旋钮手感页面](http://192.168.0.103/#knob)：自由、段落、两档、五档、回中、正反棘轮，先校准再启用。见 [模式与 API 说明](docs/haptic-modes.md)。

首次构建前配置 Wi-Fi：

```sh
mkdir -p .local
cp wifi.env.example .local/wifi.env
```

编辑 `.local/wifi.env` 填入 Wi-Fi 名称和密码，格式为 `KEY=value`，无需引号。该目录被 Git 忽略；也可通过环境变量 `MOTOR_WIFI_SSID` / `MOTOR_WIFI_PASSWORD` 覆盖。未配置凭证时仍能编译，但无法连接网络。不要提交含凭证的编译产物。

完整管理页面和 HTTP 调参服务已内置 ESP32-C3，见 [HTTP/API 使用说明](docs/http-api.md)。
默认配置 1V/8Hz，可调幅值 0.5–2V、电角频率 0–100Hz；0Hz 为通电保持。参数可通过接口直接调整，无需重新编译或烧录。
直接访问板子当前局域网地址 `http://192.168.0.102/`，电脑或手机连接同一网络即可使用，无需启动 Python 后台。地址由 DHCP 分配，可能随网络变化。
打开页面与应用参数都不会自动启动电机。
已加入 [磁编码器实时页](http://192.168.0.102/#encoder)：AS5600 接 GPIO20/SDA、GPIO21/SCL，独立任务持续采样，电机运行时也更新；运行时临时切换低延迟滤波、停机恢复原配置，不写入 OTP；旋钮手感模式使用角度反馈。
工作台新增 `#wiring`「3D 接线」：旋转/缩放、正背俯视、线路与引脚检查、透明及编码器展开视图。模型与脚本随页面打包，无 CDN 或额外服务；说明见 [3D 接线视图](docs/wiring-3d.md)。GPIO 分配不变，尺寸估计不能作为加工依据。
当前运行状态以后台实时显示为准；下方时间线保留此前串口调试历史。

完整调试总结见 [docs/motor-bringup.md](docs/motor-bringup.md)，包含接线、实验对照、当前 1V/8Hz 配置、过热问题与后续反馈方案。

**温升历史：此前已因发热停机。** 用户报告持续运行后电机“很烫”。
此前 2V 参数只验证过能转，尚未验证持续运行温升，不能视为安全连续运行参数。
侧聊再次请求提速并持续运行：当前持续模式为 1V 幅值、8Hz 电角频率；未验证长期温升。
此前 1V/2Hz 已完成 30 秒试运行，无故障，用户反馈“没有明显升温”。
随后按用户要求提至 1V/4Hz，已烧录并启动持续控制，启动前 FAULT HIGH。
用户确认 1V/4Hz“更快且平稳，没有明显升温”，可作为后续调试基准，仍未验证其他负载或长期温升。
用户随后要求“停”：向 PTY session 74141 发送 Ctrl-C 后进程以 0 退出，补发 stop 收到 STOPPED。
用户随后再次要求提速，已烧录 1V/8Hz 并重新启动持续运行，启动前 FAULT HIGH。
1V/8Hz 曾由 PTY session 24964 持续运行，用户确认“更快且平稳”；长期温升未验证。
用户随后要求停止，已向 session 24964 发送 Ctrl-C，进程以 0 退出；补发 stop 收到 STOPPED。
当前输出关闭，无持续保活进程，不会自动恢复运行。
该版本已通过 10 项控制测试及 clippy 并烧录；用户确认冷却及接线就绪后，启动前读到 FAULT LOW。
随后用户确认驱动板重新上电且红灯稳定，复查得到 FAULT HIGH。
执行 `continuous --seconds 3`，收到 `RUN continuous 1V 1Hz`，完成后正常返回 STOPPED，脚本退出码 0。
当前已停止，未报告故障；用户确认“转得更快且平稳”，3 秒测试不构成长期温升验证。
当前控制没有绕组电流或电机温度反馈；继续试验前应断开 12V 冷却，并测量电流/绕组参数、降低幅值作短时验证。

默认固件是 `motor-driver`，原 Wi-Fi 程序保留在 `src/bin/main.rs`，
可用 `cargo run --features wifi-demo --bin esp32c3-proj` 单独编译烧录。
这一版只做三相正弦 PWM 开环换相，不依赖 AS5600，也不实现闭环力反馈。

## 接线（断电操作）

| ESP32-C3 | 驱动板 |
|---|---|
| GPIO4 | IN1 |
| GPIO5 | IN2 |
| GPIO6 | IN3 |
| GPIO7 | EN |
| GND | GND |
| GPIO10（诊断，可选） | nFT / nFAULT |

电机三根相线连接 M1/M2/M3。驱动板使用 12V 适配器，C3 使用 USB；
两边共地，正电源不互接。按驱动板实际丝印确认端子，不按针脚位置猜。
本程序假定 EN 高有效，驱动支持三路 PWM。
DRV8313 的 nSP/nSLEEP 和 nRT/nRESET 必须为高电平；如果模块没有板载上拉，
可将这两个逻辑输入接到 C3 的 3V3，不能把驱动板的 3.3V 输出与 C3 的 3V3 并联。
在 12V 和 USB 都断开时修改接线。
建议 EN 与 GND 之间有 10kΩ 下拉，让主控复位或断电时也保持关闭。

## 编译和烧录

```sh
cargo build --release --bin motor-driver
espflash flash --port /dev/cu.usbmodem21201 --non-interactive target/riscv32imc-unknown-none-elf/release/motor-driver
espflash monitor --port /dev/cu.usbmodem21201 --non-interactive
```

串口每条命令以换行结束：

- `?`：查看状态。
- `diag`：保持 EN 关闭，以 50% 占空比测试 20ms，读取 GPIO4/5/6 的实际电平跳变次数及 nFT 低电平采样次数，随后归零。
- `fault`：读取 GPIO10 上的 nFT 电平。LOW 表示故障；HIGH 必须在确认 nFT 已接好后才有意义。
- `faultwire`：关闭 EN 后分别启用 GPIO10 的弱上拉/弱下拉，等待各 2ms 后采样并恢复上拉。
  UP=1/DOWN=0 与浮空输入相同，也可能是模块没有外部上拉或上拉很弱；
  UP=1/DOWN=1 与外部较强上拉一致，但也可能是接到了其他高电平节点，不能单凭此认定驱动器正常。
- `arm`：允许接下来 10 秒内启动一次测试，不立即通电。
- `f`：正向测试；必须先 `arm`。
- `r`：反向测试；必须重新 `arm`。
- `c`：1V 幅值、8Hz 电角频率持续正转；必须先 `arm`。每隔小于 500ms 发送 `k` 保活，缺失则自动停机。
- `k`：只续期正在运行的持续模式，回复 `ALIVE`；不能启动电机或复活已过期的运行。
- `s` / `stop` / Ctrl-C / Esc：立即关闭 EN。

也可以用已有 pyserial 的 Python 直接控制（运行前关闭占用端口的串口监视器）：

```sh
python3 scripts/motor.py status
python3 scripts/motor.py diag
python3 scripts/motor.py fault
python3 scripts/motor.py faultwire
python3 scripts/motor.py forward
python3 scripts/motor.py reverse
python3 scripts/motor.py continuous --seconds 3
python3 scripts/motor.py continuous
python3 scripts/motor.py stop
```

`forward` / `reverse` 自动发送 arm 和转动指令，等待 1 秒停机并验证最终状态。
`continuous` 在同一个串口连接内持续发送保活；Ctrl-C 停机，USB 断开或主机停止保活也会停机。
`--seconds 3` 用于 3 秒试运行；省略时持续运行。运行期间不要从另一个程序同时打开串口。
脚本先通过状态查询等待固件就绪，避免复位启动期间丢失首条命令。
只有状态查询会重试，转动命令不会自动重试。
`diag` 自检不会使能电机。20kHz PWM 在 20ms 内应各出现约 800 次跳变；
计数只证明主控引脚存在信号，不能证明驱动板收到信号、供电正常或电机输出正常。
`nFTlow` 是 EN 关闭期间采到的低电平次数；非零说明故障低电平也会在未使能功率输出时出现，
需检查故障线连接、信号干扰或驱动器本身故障。零不能排除漏采的短脉冲或仅运行时出现的故障。
固件为 GPIO10 启用内部上拉。nFT 拉低时拒绝启动，运行时拉低则立即关闭 EN；
未接 nFT 时，高电平来自主控上拉，不能据此认定驱动板正常。

上电不自动转动。f/r 脉冲测试最长 1 秒，首尾各 0.2 秒渐变，之后关闭 EN。
持续模式使用独立的 1V 相电压幅值，前 0.2 秒渐增，电角频率为 8Hz；
500ms 保活租期或更新间隔超限都会关闭输出，故障保护同样有效。
HTTP 动态模式按更新时间增量累积并环绕电角度，调频不重置相位。
前 0.2 秒保持磁场方向不变作启动对齐；之后 f/r 脉冲模式为 0.5Hz，持续模式为 8Hz 电角频率。
启动对齐为开环操作，不表示已通过编码器验证对齐成功。
运行中重复 f/r 不延长截止时间。更新停顿超过 20ms、未知命令、输入溢出或 panic 都关闭 EN。
USB 输出采用非阻塞方式，避免断开串口后日志阻塞自动停机。

电源假定 12V，PWM 为 20kHz / 11bit，更新频率约 2kHz。
按用户要求作短时诊断，电压指令幅值为 12.0V，电角频率 0.5Hz（机械转速取决于极对数）。
12V 母线下，居中正弦调制的线性幅值上限为 6V；12V 指令进入过调制。
PWM 比较值在取整后钳位到 0..2047，波形会削顶，不能称为实测或完整的 12V 相电压。
12.0V 指令仅用于这次 1 秒脉冲测试，未确认绕组阻值和电流前不能作为持续运行参数。
运行故障回包中的 `t=...us` 记录启动至捕获低电平的时间，停机在回包前执行。
用户新提供的电机工作电压为 7.2V，尚未确认与之前 12V 标签信息的关系。
电压指令和 PWM 钳位不是实测电流保护；未知低阻电机仍需限流电源。
如果电机只抖动或不转，先确认接线、驱动故障、绕组参数再调整幅值，不直接施加满 12V。
没有编码器反馈，软件显示 RUN 只能证明输出已启动，不能证明电机实际转动。

## 最近一次实机诊断

2026-10-18（Asia/Shanghai）：烧录 8V 指令限幅版本后，仅发送一次 `forward`。
串口先返回 `RUN 1s 0.5Hz cmd=8V clipped; align 0.2s`，随后立即返回
`FAULT LOW STOPPED; GPIO10/nFT asserted`，主机脚本以错误退出并补发停止命令。
这次因故障提前终止，没有运行满 1 秒；电机实际动作仍需用户观察确认。
随后重新打开串口读到 `STATUS stopped` 和 `FAULT HIGH`。
这只能证明 GPIO10 在启动期间出现低电平，不能单凭 nFT 区分欠压、过流、过热或接线干扰。
不要自动重试或屏蔽故障保护，下一步检查实际供电和接线。
用户随后明确要求重试一次：启动前 nFT 为 HIGH，第二次启动也返回 `FAULT LOW STOPPED`。
用户观察反馈为“好像动了；有点声音”，尚未确认持续旋转。
加入被动故障采样后，EN=0 下执行一次 `diag`：`edges=801/801/800 nFTlow=0`。
故障未在这次 20ms 输入 PWM 自检中出现，尚不能区分负载、供电、驱动器或运行时干扰问题。
下一步需要用户断电并断开 M1/M2/M3 电机相线，再对无电机负载的驱动板作一次短时测试。
用户确认“电机线已拔”后执行一次相同 8V 指令空载测试：启动前 `FAULT HIGH`，
`forward` 脚本运行到自动停机并以 0 退出，末尾确认 `STATUS stopped`；停机后再次读到 `FAULT HIGH`。
这次空载测试未报告故障；与两次带电机启动立即故障形成对照，问题与接入电机后的负载或干扰有关。
不能据此认定驱动功率级完全正常，也不能区分电机/相线短路、带载供电掉压及运行时干扰。
用户随后坚持增加指令幅值：已编译、通过 7 项控制测试及 clippy，并烧录 12V 指令限幅版本。
串口确认 `STATUS stopped ... bus=12V cmd=12V clipped 0.5Hz`，补发 `stop` 收到 `STOPPED`。
此版本尚未启动测试；最后确认的实物状态仍为电机线已拔，等待用户确认接回和恢复供电。
用户之后确认“已接回并恢复供电”。启动前读取到 `FAULT LOW`，因此没有发送 `forward`；
补发 `stop` 确认输出关闭。需要先对驱动板 12V 断电再上电，并复查故障脚。
用户确认重新上电后：启动前仍读到 `FAULT LOW`，未发送转动命令。
随后保持 EN=0 执行被动 `faultwire`：`UP=0 DOWN=0 RESTORED=UP EN=0`，低电平仍存在。
等待用户反馈本次上电后的驱动板电源灯状态，以检查供电是否明显异常。
用户随后发现电源线断开并确认已重新连接。故障脚恢复为 HIGH。
执行一次 12V 指令测试，捕获 `FAULT LOW STOPPED t=110596us; GPIO10/nFT`。
用户报告“动了”，但此次仅通电约 0.11 秒，尚在 0.2 秒对齐阶段，不能视为持续旋转成功。
按用户要求增加独立的 2V 低幅值持续模式，保留故障即停并增加主机保活停机机制。
此版本通过 10 项 Rust 控制测试、4 项主机脚本测试、release 构建与 clippy。
实机执行 `continuous --seconds 3`，完成 3 秒试运行，未报告故障，正常返回 `STOPPED`。
随后执行 `python3 scripts/motor.py continuous` 持续运行；用户明确确认“持续慢慢转”。
已验证可运行配置：12V 母线，2V 相电压幅值，20kHz 三相 PWM，0.5Hz 电角频率。
当前持续控制进程使用本次会话 PTY session 90744；停止时向该会话发送 Ctrl-C，脚本 finally 补发 stop。
不要为读取状态另开同一串口，以免复位或干扰正在运行的控制连接。
后续用户报告电机很烫：检查 session 90744 发现进程已以错误退出，日志为
`FAULT LOW STOPPED t=169005243us; GPIO10/nFT`，即运行约 169 秒时捕获故障低电平。
具体故障类型未确定，不能将驱动芯片保护当成电机绕组的温度保护。
随后补发 `stop` 并收到 `STOPPED`，当前不再持续运行，也未自动重试。

## 控制逻辑测试（不连接硬件）

从项目目录以外执行，避免继承嵌入式目标和 build-std 设置：

```sh
cd /tmp
cargo +stable test --manifest-path /Users/lishuang/workDir/esp32c3-proj/motor-control/Cargo.toml
```

参考：[SimpleFOCMini 接线](https://docs.simplefoc.com/mini_v11_connect_hardware)。
