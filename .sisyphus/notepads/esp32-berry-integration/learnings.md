## Berry VM Integration (Task 13)

### Implementation
- Added Berry library via GitHub URL to platformio.ini
- Created berry_vm.cpp/h with minimal native function bindings
- Exposed digitalWrite, digitalRead, analogRead to Berry scripts
- Integrated berry_init() into main.cpp setup()
- Created test-qemu-berry-init.sh for validation

### Key Decisions
- Used direct GitHub repo URL instead of PlatformIO registry (no official Berry package)
- Minimal API surface: only GPIO/ADC hardware control functions
- No filesystem access or complex stdlib to keep footprint small (~40KB Flash)
- Native function bindings follow Berry's C API pattern (be_regfunc)

### Technical Notes
- LSP errors for Arduino.h and Berry headers are expected (not in LSP include path)
- Berry VM initialized once in setup(), reused for all script executions
- Function signatures: digitalWrite(pin, value), digitalRead(pin), analogRead(pin)
