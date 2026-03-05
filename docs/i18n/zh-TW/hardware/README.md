# 硬體與外設文件

適用於開發板整合、韌體流程與外設架構。

ZeroClaw 的硬體子系統透過 `Peripheral` trait 實作對微控制器與外設的直接控制。每塊開發板會暴露 GPIO、ADC 與感測器操作的工具，允許代理驅動的硬體互動，支援 STM32 Nucleo、Raspberry Pi 和 ESP32 等開發板。完整架構請參閱 [hardware-peripherals-design.md](../hardware-peripherals-design.md)。

## 入口文件

- 架構與外設模型：[../hardware-peripherals-design.md](../hardware-peripherals-design.md)
- Raspberry Pi Zero W 建置：[raspberry-pi-zero-w-build.md](raspberry-pi-zero-w-build.md)
- 新增開發板／工具：[../adding-boards-and-tools.md](../adding-boards-and-tools.md)
- Nucleo 設定：[../nucleo-setup.md](../nucleo-setup.md)
- Arduino Uno R4 WiFi 設定：[../arduino-uno-q-setup.md](../arduino-uno-q-setup.md)

## 資料手冊

- 資料手冊索引：[../datasheets/README.md](../datasheets/README.md)
- STM32 Nucleo-F401RE：[../datasheets/nucleo-f401re.md](../datasheets/nucleo-f401re.md)
- Arduino Uno：[../datasheets/arduino-uno.md](../datasheets/arduino-uno.md)
- ESP32：[../datasheets/esp32.md](../datasheets/esp32.md)
