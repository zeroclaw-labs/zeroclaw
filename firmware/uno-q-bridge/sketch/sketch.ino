// ZeroClaw Bridge — expose digitalWrite/digitalRead for agent GPIO control
// SPDX-License-Identifier: MPL-2.0

#include "Arduino_RouterBridge.h"

void gpio_write(int pin, int value) {
  pinMode(pin, OUTPUT);
  digitalWrite(pin, value ? HIGH : LOW);
}

int gpio_read(int pin) {
  pinMode(pin, INPUT);
  return digitalRead(pin);
}

void setup() {
  Bridge.begin();
  Bridge.provide("digitalWrite", gpio_write);
  Bridge.provide("digitalRead", gpio_read);
}

void loop() {
  Bridge.update();
}
