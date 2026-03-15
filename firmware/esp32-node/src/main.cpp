#include <Arduino.h>
#include <WiFi.h>
#include <PubSubClient.h>
#include <ArduinoJson.h>
#include <SPIFFS.h>
#include "berry_vm.h"
#include "script_cache.h"

const char* ssid = "SSID";
const char* password = "PASSWORD";
const char* mqtt_server = "broker.example.com";
const char* node_id = "esp32-001";

WiFiClient espClient;
PubSubClient client(espClient);

const char* ALLOWED_COMMANDS[] = {"gpio_read", "gpio_write", "adc_read", "script_cache", "script_execute", "script_list", "script_delete"};
const int ALLOWED_COMMANDS_COUNT = 7;

bool registered = false;
unsigned long last_heartbeat = 0;
const unsigned long HEARTBEAT_INTERVAL = 30000;

void setup_wifi() {
  WiFi.begin(ssid, password);
  while (WiFi.status() != WL_CONNECTED) {
    delay(500);
  }
}

bool is_command_allowed(const char* cmd) {
  for (int i = 0; i < ALLOWED_COMMANDS_COUNT; i++) {
    if (strcmp(cmd, ALLOWED_COMMANDS[i]) == 0) return true;
  }
  return false;
}

void send_heartbeat() {
  StaticJsonDocument<128> doc;
  doc["timestamp"] = millis();
  
  char buffer[128];
  serializeJson(doc, buffer);
  
  char topic[64];
  snprintf(topic, sizeof(topic), "zeroclaw/nodes/%s/heartbeat", node_id);
  client.publish(topic, buffer, false);
}

void send_register() {
  StaticJsonDocument<512> doc;
  doc["type"] = "register";
  doc["node_id"] = node_id;
  
  JsonArray caps = doc.createNestedArray("capabilities");
  
  JsonObject cap1 = caps.createNestedObject();
  cap1["name"] = "gpio_read";
  cap1["description"] = "Read digital GPIO pin state";
  JsonObject params1 = cap1.createNestedObject("parameters");
  params1["type"] = "object";
  JsonObject props1 = params1.createNestedObject("properties");
  JsonObject pin1 = props1.createNestedObject("pin");
  pin1["type"] = "integer";
  
  JsonObject cap2 = caps.createNestedObject();
  cap2["name"] = "gpio_write";
  cap2["description"] = "Write digital GPIO pin state";
  JsonObject params2 = cap2.createNestedObject("parameters");
  params2["type"] = "object";
  JsonObject props2 = params2.createNestedObject("properties");
  JsonObject pin2 = props2.createNestedObject("pin");
  pin2["type"] = "integer";
  JsonObject val2 = props2.createNestedObject("value");
  val2["type"] = "integer";
  
  JsonObject cap3 = caps.createNestedObject();
  cap3["name"] = "adc_read";
  cap3["description"] = "Read analog ADC value";
  JsonObject params3 = cap3.createNestedObject("parameters");
  params3["type"] = "object";
  JsonObject props3 = params3.createNestedObject("properties");
  JsonObject pin3 = props3.createNestedObject("pin");
  pin3["type"] = "integer";
  
  JsonObject cap4 = caps.createNestedObject();
  cap4["name"] = "script_cache";
  cap4["description"] = "Cache Berry script to SPIFFS";
  
  JsonObject cap5 = caps.createNestedObject();
  cap5["name"] = "script_execute";
  cap5["description"] = "Execute cached Berry script";
  
  JsonObject cap6 = caps.createNestedObject();
  cap6["name"] = "script_list";
  cap6["description"] = "List cached scripts";
  
  JsonObject cap7 = caps.createNestedObject();
  cap7["name"] = "script_delete";
  cap7["description"] = "Delete cached script";
  
  char buffer[512];
  serializeJson(doc, buffer);
  
  char topic[64];
  snprintf(topic, sizeof(topic), "zeroclaw/nodes/%s/register", node_id);
  client.publish(topic, buffer, false);
  registered = true;
}

void publish_result(const char* request_id, bool success, const char* data, const char* error) {
  StaticJsonDocument<256> doc;
  doc["request_id"] = request_id;
  doc["success"] = success;
  if (data) doc["data"] = data;
  if (error) doc["error"] = error;
  
  char buffer[256];
  serializeJson(doc, buffer);
  
  char topic[64];
  snprintf(topic, sizeof(topic), "zeroclaw/nodes/%s/result", node_id);
  client.publish(topic, buffer);
}

void handle_gpio_read(const char* request_id, JsonObject params) {
  if (!params.containsKey("pin")) {
    publish_result(request_id, false, nullptr, "missing pin parameter");
    return;
  }
  
  int pin = params["pin"];
  pinMode(pin, INPUT);
  int value = digitalRead(pin);
  
  char data[16];
  snprintf(data, sizeof(data), "%d", value);
  publish_result(request_id, true, data, nullptr);
}

void handle_gpio_write(const char* request_id, JsonObject params) {
  if (!params.containsKey("pin") || !params.containsKey("value")) {
    publish_result(request_id, false, nullptr, "missing pin or value parameter");
    return;
  }
  
  int pin = params["pin"];
  int value = params["value"];
  pinMode(pin, OUTPUT);
  digitalWrite(pin, value);
  
  publish_result(request_id, true, "ok", nullptr);
}

void handle_adc_read(const char* request_id, JsonObject params) {
  if (!params.containsKey("pin")) {
    publish_result(request_id, false, nullptr, "missing pin parameter");
    return;
  }
  
  int pin = params["pin"];
  int value = analogRead(pin);
  
  char data[16];
  snprintf(data, sizeof(data), "%d", value);
  publish_result(request_id, true, data, nullptr);
}

void callback(char* topic, byte* payload, unsigned int length) {
  StaticJsonDocument<512> doc;
  DeserializationError error = deserializeJson(doc, payload, length);
  
  if (error) {
    Serial.println("JSON parse failed");
    return;
  }
  
  const char* cmd = doc["command"];
  const char* request_id = doc["request_id"];
  
  if (!cmd || !request_id) {
    Serial.println("Missing command or request_id");
    return;
  }
  
  if (!is_command_allowed(cmd)) {
    publish_result(request_id, false, nullptr, "command not allowed");
    return;
  }
  
  JsonObject params = doc["params"];
  
  if (strcmp(cmd, "gpio_read") == 0) {
    handle_gpio_read(request_id, params);
  } else if (strcmp(cmd, "gpio_write") == 0) {
    handle_gpio_write(request_id, params);
  } else if (strcmp(cmd, "adc_read") == 0) {
    handle_adc_read(request_id, params);
  } else if (strcmp(cmd, "script_cache") == 0) {
    const char* script_id = params["script_id"];
    const char* code = params["code"];
    bool success = script_cache_save(script_id, code);
    publish_result(request_id, success, success ? "cached" : nullptr, success ? nullptr : "save failed");
  } else if (strcmp(cmd, "script_execute") == 0) {
    const char* script_id = params["script_id"];
    bool success = script_cache_execute(script_id);
    publish_result(request_id, success, success ? "executed" : nullptr, success ? nullptr : "exec failed");
  } else if (strcmp(cmd, "script_list") == 0) {
    script_cache_list();
    publish_result(request_id, true, "listed", nullptr);
  } else if (strcmp(cmd, "script_delete") == 0) {
    const char* script_id = params["script_id"];
    bool success = script_cache_delete(script_id);
    publish_result(request_id, success, success ? "deleted" : nullptr, success ? nullptr : "delete failed");
  }
}

void reconnect() {
  while (!client.connected()) {
    if (client.connect(node_id)) {
      char topic[64];
      snprintf(topic, sizeof(topic), "zeroclaw/nodes/%s/invoke", node_id);
      client.subscribe(topic);
      send_register();
    } else {
      delay(5000);
    }
  }
}

void setup() {
  Serial.begin(115200);
  if (!SPIFFS.begin(true)) {
    Serial.println("SPIFFS mount failed");
    return;
  }
  berry_init();
  setup_wifi();
  client.setServer(mqtt_server, 1883);
  client.setCallback(callback);
}

void loop() {
  if (!client.connected()) {
    reconnect();
  }
  client.loop();
  
  unsigned long now = millis();
  if (now - last_heartbeat >= HEARTBEAT_INTERVAL) {
    send_heartbeat();
    last_heartbeat = now;
  }
}
