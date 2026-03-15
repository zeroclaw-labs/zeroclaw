#ifndef SCRIPT_CACHE_H
#define SCRIPT_CACHE_H

bool script_cache_save(const char* script_id, const char* code);
bool script_cache_execute(const char* script_id);
void script_cache_list();
bool script_cache_delete(const char* script_id);

#endif
