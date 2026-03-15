# Script Cache Lifecycle Tests

# Test 7: script_cache - store script
var script_content = "print('cached script')"
script.cache("test_script", script_content)
print("test_script_cache: PASS")

# Test 8: script_execute - run cached script
script.execute("test_script")
print("test_script_execute: PASS")
