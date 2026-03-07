# ZeroClaw Pico firmware — serial JSON protocol handler
# MicroPython — Raspberry Pi Pico (RP2040)
#
# Wire protocol:
#   Host → Device:  {"cmd":"gpio_write","params":{"pin":25,"value":1}}\n
#   Device → Host:  {"ok":true,"data":{"pin":25,"value":1,"state":"HIGH"}}\n
#
# Pin direction policy:
#   gpio_write always configures the pin as OUTPUT and caches it.
#   gpio_read uses the cached Pin object if one already exists, so a pin
#   that was set via gpio_write retains its OUTPUT direction — it is NOT
#   reconfigured to INPUT.  If no cached Pin exists the pin is opened as
#   INPUT and the new Pin is cached for subsequent reads.

import sys
import json
from machine import Pin

# Onboard LED — GPIO 25 on Pico 1
led = Pin(25, Pin.OUT)

# Cache of Pin objects keyed by pin number (excludes the onboard LED on 25).
# gpio_write stores pins as OUTPUT; gpio_read reuses the existing Pin if one
# is cached rather than clobbering its direction.
pins_cache = {}

def handle(msg):
    cmd    = msg.get("cmd")
    params = msg.get("params", {})

    if cmd == "ping":
        # data.firmware must equal "zeroclaw" for ping_handshake() to pass
        return {"ok": True, "data": {"firmware": "zeroclaw", "version": "1.0.0"}}

    elif cmd == "gpio_write":
        pin_num = params.get("pin")
        value   = params.get("value")
        if pin_num is None or value is None:
            return {"ok": False, "error": "missing pin or value"}
        # Validate/cast pin_num to int (JSON may deliver it as float or string)
        try:
            pin_num = int(pin_num)
        except (TypeError, ValueError):
            return {"ok": False, "error": "invalid pin"}
        if pin_num < 0:
            return {"ok": False, "error": "invalid pin"}
        # Normalize value: accept bool or int, must resolve to 0 or 1.
        if isinstance(value, bool):
            value = int(value)
        if not isinstance(value, int) or value not in (0, 1):
            return {"ok": False, "error": "invalid value: must be 0 or 1"}
        if pin_num == 25:
            led.value(value)
        else:
            # Reuse a cached Pin object when available to avoid repeated
            # allocations; re-initialise direction to OUT in case it was
            # previously opened as IN by gpio_read.
            if pin_num in pins_cache:
                pins_cache[pin_num].init(mode=Pin.OUT)
            else:
                pins_cache[pin_num] = Pin(pin_num, Pin.OUT)
            pins_cache[pin_num].value(value)
        state = "HIGH" if value == 1 else "LOW"
        return {"ok": True, "data": {"pin": pin_num, "value": value, "state": state}}

    elif cmd == "gpio_read":
        pin_num = params.get("pin")
        if pin_num is None:
            return {"ok": False, "error": "missing pin"}
        # Validate/cast pin_num to int
        try:
            pin_num = int(pin_num)
        except (TypeError, ValueError):
            return {"ok": False, "error": "invalid pin"}
        if pin_num < 0:
            return {"ok": False, "error": "invalid pin"}
        value = led.value() if pin_num == 25 else (
            pins_cache[pin_num].value() if pin_num in pins_cache
            else pins_cache.setdefault(pin_num, Pin(pin_num, Pin.IN)).value()
        )
        state = "HIGH" if value == 1 else "LOW"
        return {"ok": True, "data": {"pin": pin_num, "value": value, "state": state}}

    else:
        return {"ok": False, "error": "unknown cmd: {}".format(cmd)}

while True:
    try:
        line = sys.stdin.readline().strip()
        if not line:
            continue
        msg    = json.loads(line)
        result = handle(msg)
        print(json.dumps(result))
    except (ValueError, KeyError, TypeError, OSError, AttributeError) as e:
        # ValueError      — json.loads() on malformed input
        # KeyError        — unexpected missing key in a message dict
        # TypeError       — wrong type in an operation
        # OSError         — GPIO/hardware errors from Pin()/Pin.value()
        # AttributeError  — msg.get(...) called on non-dict JSON value
        # Any other exception propagates so bugs surface during development.
        print(json.dumps({"ok": False, "error": str(e)}))
