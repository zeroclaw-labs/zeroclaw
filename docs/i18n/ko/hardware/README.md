# 하드웨어 및 주변장치 문서

board 통합, firmware 흐름, 주변장치 아키텍처에 관한 문서입니다.

ZeroClaw의 하드웨어 서브시스템은 `Peripheral` trait를 통해 마이크로컨트롤러와 주변장치를 직접 제어할 수 있습니다. 각 board는 GPIO, ADC, 센서 작업을 위한 도구를 제공하며, STM32 Nucleo, Raspberry Pi, ESP32와 같은 board에서 에이전트 기반 하드웨어 상호작용을 가능하게 합니다. 전체 아키텍처는 [hardware-peripherals-design.md](hardware-peripherals-design.md)를 참조하십시오.

## 시작 지점

- 아키텍처 및 주변장치 모델: [hardware-peripherals-design.md](hardware-peripherals-design.md)
- 새 board/도구 추가: [../contributing/adding-boards-and-tools.md](../contributing/adding-boards-and-tools.md)
- Nucleo 설정: [nucleo-setup.md](nucleo-setup.md)
- Arduino Uno R4 WiFi 설정: [arduino-uno-q-setup.md](arduino-uno-q-setup.md)

## 데이터시트

- 데이터시트 목록: [datasheets](datasheets)
- STM32 Nucleo-F401RE: [datasheets/nucleo-f401re.md](datasheets/nucleo-f401re.md)
- Arduino Uno: [datasheets/arduino-uno.md](datasheets/arduino-uno.md)
- ESP32: [datasheets/esp32.md](datasheets/esp32.md)
