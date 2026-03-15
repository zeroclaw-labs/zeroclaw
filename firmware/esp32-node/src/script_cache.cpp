#include <Arduino.h>
#include <SPIFFS.h>
#include "script_cache.h"
#include "berry_vm.h"

bool script_cache_save(const char* script_id, const char* code) {
  char path[64];
  snprintf(path, sizeof(path), "/scripts/%s.be", script_id);
  
  File f = SPIFFS.open(path, "w");
  if (!f) return false;
  
  f.print(code);
  f.close();
  return true;
}

bool script_cache_execute(const char* script_id) {
  char path[64];
  snprintf(path, sizeof(path), "/scripts/%s.be", script_id);
  
  File f = SPIFFS.open(path, "r");
  if (!f) return false;
  
  String code = f.readString();
  f.close();
  
  return berry_execute(code.c_str());
}

void script_cache_list() {
  File root = SPIFFS.open("/scripts");
  if (!root || !root.isDirectory()) return;
  
  File file = root.openNextFile();
  while (file) {
    Serial.println(file.name());
    file = root.openNextFile();
  }
}

bool script_cache_delete(const char* script_id) {
  char path[64];
  snprintf(path, sizeof(path), "/scripts/%s.be", script_id);
  return SPIFFS.remove(path);
}
