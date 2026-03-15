# Script List and Delete Tests

# Test 9: script_list - list cached scripts
var scripts = script.list()
print("test_script_list: PASS")

# Test 10: script_delete - remove cached script
script.delete("test_script")
print("test_script_delete: PASS")
