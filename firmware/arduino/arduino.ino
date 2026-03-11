/*
 * ZeroClaw Arduino Uno Firmware
 *
 * Listens for JSON commands on Serial (115200 baud), executes gpio_read/gpio_write,
 * responds with JSON. Compatible with ZeroClaw SerialPeripheral protocol.
 *
 * Protocol (newline-delimited JSON):
 *   Request:  {"id":"1","cmd":"gpio_write","args":{"pin":13,"value":1}}
 *   Response: {"id":"1","ok":true,"result":"done"}
 *
 * Arduino Uno: Pin 13 has built-in LED. Digital pins 0-13 supported.
 *
 * 1. Open in Arduino IDE
 * 2. Select Board: Arduino Uno
 * 3. Select correct Port (Tools -> Port)
 * 4. Upload
 */

#define BAUDRATE 115200
#define MAX_LINE 256

char lineBuf[MAX_LINE];
int lineLen = 0;

// Parse integer from JSON: "pin":13 or "value":1
int parseArg(const char* key, const char* json) {
  char search[32];
  snprintf(search, sizeof(search), "\"%s\":", key);
  const char* p = strstr(json, search);
  if (!p) return -1;
  p += strlen(search);
  return atoi(p);
}

// Extract "id" for response
void copyId(char* out, int outLen, const char* json) {
  const char* p = strstr(json, "\"id\":\"");
  if (!p) {
    out[0] = '0';
    out[1] = '\0';
    return;
  }
  p += 6;
  int i = 0;
  while (i < outLen - 1 && *p && *p != '"') {
    out[i++] = *p++;
  }
  out[i] = '\0';
}

// Check if cmd is present
bool hasCmd(const char* json, const char* cmd) {
  char search[64];
  snprintf(search, sizeof(search), "\"cmd\":\"%s\"", cmd);
  return strstr(json, search) != NULL;
}

void handleLine(const char* line) {
  char idBuf[16];
  copyId(idBuf, sizeof(idBuf), line);

  if (hasCmd(line, "ping")) {
    Serial.print("{\"id\":\"");
    Serial.print(idBuf);
    Serial.println("\",\"ok\":true,\"result\":\"pong\"}");
    return;
  }

  // Phase C: Dynamic discovery — report GPIO pins and LED pin
  if (hasCmd(line, "capabilities")) {
    Serial.print("{\"id\":\"");
    Serial.print(idBuf);
    Serial.print("\",\"ok\":true,\"result\":\"{\\\"gpio\\\":[0,1,2,3,4,5,6,7,8,9,10,11,12,13],\\\"led_pin\\\":13}\"}");
    Serial.println();
    return;
  }

  if (hasCmd(line, "gpio_read")) {
    int pin = parseArg("pin", line);
    if (pin < 0 || pin > 13) {
      Serial.print("{\"id\":\"");
      Serial.print(idBuf);
      Serial.print("\",\"ok\":false,\"result\":\"\",\"error\":\"Invalid pin ");
      Serial.print(pin);
      Serial.println("\"}");
      return;
    }
    pinMode(pin, INPUT);
    int val = digitalRead(pin);
    Serial.print("{\"id\":\"");
    Serial.print(idBuf);
    Serial.print("\",\"ok\":true,\"result\":\"");
    Serial.print(val);
    Serial.println("\"}");
    return;
  }

  if (hasCmd(line, "gpio_write")) {
    int pin = parseArg("pin", line);
    int value = parseArg("value", line);
    if (pin < 0 || pin > 13) {
      Serial.print("{\"id\":\"");
      Serial.print(idBuf);
      Serial.print("\",\"ok\":false,\"result\":\"\",\"error\":\"Invalid pin ");
      Serial.print(pin);
      Serial.println("\"}");
      return;
    }
    pinMode(pin, OUTPUT);
    digitalWrite(pin, value ? HIGH : LOW);
    Serial.print("{\"id\":\"");
    Serial.print(idBuf);
    Serial.println("\",\"ok\":true,\"result\":\"done\"}");
    return;
  }

  // Unknown command
  Serial.print("{\"id\":\"");
  Serial.print(idBuf);
  Serial.println("\",\"ok\":false,\"result\":\"\",\"error\":\"Unknown command\"}");
}

void setup() {
  Serial.begin(BAUDRATE);
  lineLen = 0;
}

void loop() {
  while (Serial.available()) {
    char c = Serial.read();
    if (c == '\n' || c == '\r') {
      if (lineLen > 0) {
        lineBuf[lineLen] = '\0';
        handleLine(lineBuf);
        lineLen = 0;
      }
    } else if (lineLen < MAX_LINE - 1) {
      lineBuf[lineLen++] = c;
    } else {
      lineLen = 0;  // Overflow, discard
    }
  }
}
