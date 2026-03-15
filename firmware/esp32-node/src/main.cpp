#include <Arduino.h>
#include <WiFi.h>
#include <PubSubClient.h>
#include <ArduinoJson.h>

const char* ssid = "SSID";
const char* password = "PASSWORD";
const char* mqtt_server = "broker.example.com";
const char* node_id = "esp32-001";

WiFiClient espClient;
PubSubClient client(espClient);

const char* ALLOWED_COMMANDS[] = {"gpio_read", "gpio_write", "adc_read"};
const int ALLOWED_COMMANDS_COUNT = 3;

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
  }
}

void reconnect() {
  while (!client.connected()) {
    if (client.connect(node_id)) {
      char topic[64];
      snprintf(topic, sizeof(topic), "zeroclaw/nodes/%s/invoke", node_id);
      client.subscribe(topic);
    } else {
      delay(5000);
    }
  }
}

void setup() {
  Serial.begin(115200);
  setup_wifi();
  client.setServer(mqtt_server, 1883);
  client.setCallback(callback);
}

void loop() {
  if (!client.connected()) {
    reconnect();
  }
  client.loop();
}
