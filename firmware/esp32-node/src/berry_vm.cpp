#include "berry_vm.h"
#include <Arduino.h>
#include "berry.h"

static bvm *vm = nullptr;

// Native function: digitalWrite
static int m_digital_write(bvm *vm) {
    int argc = be_top(vm);
    if (argc >= 2 && be_isint(vm, 1) && be_isint(vm, 2)) {
        int pin = be_toint(vm, 1);
        int value = be_toint(vm, 2);
        pinMode(pin, OUTPUT);
        digitalWrite(pin, value);
        be_return_nil(vm);
    }
    be_raise(vm, "type_error", "digitalWrite(pin:int, value:int)");
}

// Native function: digitalRead
static int m_digital_read(bvm *vm) {
    int argc = be_top(vm);
    if (argc >= 1 && be_isint(vm, 1)) {
        int pin = be_toint(vm, 1);
        pinMode(pin, INPUT);
        int value = digitalRead(pin);
        be_pushint(vm, value);
        be_return(vm);
    }
    be_raise(vm, "type_error", "digitalRead(pin:int)");
}

// Native function: analogRead
static int m_analog_read(bvm *vm) {
    int argc = be_top(vm);
    if (argc >= 1 && be_isint(vm, 1)) {
        int pin = be_toint(vm, 1);
        int value = analogRead(pin);
        be_pushint(vm, value);
        be_return(vm);
    }
    be_raise(vm, "type_error", "analogRead(pin:int)");
}

void berry_init() {
    vm = be_vm_new();
    
    be_regfunc(vm, "digitalWrite", m_digital_write);
    be_regfunc(vm, "digitalRead", m_digital_read);
    be_regfunc(vm, "analogRead", m_analog_read);
}

void berry_execute(const char* script) {
    if (vm && script) {
        be_loadstring(vm, script);
        be_pcall(vm, 0);
    }
}
