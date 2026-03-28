#!/usr/bin/env python3
import subprocess
import json
import os
import time
from pathlib import Path

# Configuration
BINARY = "./bazel-bin/zeroclaw"
PROFILE = "default"
SESSION_PARIS = "paris-test-bench"
SESSION_FASTAPI = "fastapi-test-bench"

def run_zeroclaw(message, session, provider=None, interactive_input=None):
    if interactive_input:
        cmd = [BINARY, "--profile", PROFILE, "agent", "--session", session]
    else:
        cmd = [BINARY, "--profile", PROFILE, "agent", "--session", session, "-m", message]
        
    if provider:
        cmd.extend(["-p", provider])
    
    print(f"Executing: {' '.join(cmd)}")
    
    if interactive_input:
        result = subprocess.run(cmd, input=interactive_input, capture_output=True, text=True)
    else:
        result = subprocess.run(cmd, capture_output=True, text=True)
        
    return result.stdout, result.stderr

def test_paris_itinerary():
    print("\n--- Scenario 1: Paris Itinerary ---")
    message = "Generate a detailed itinerary for a one-day trip to Paris, focusing on art and gastronomy. Include the Louvre and a recommendation for a croissant."
    stdout, stderr = run_zeroclaw(message, SESSION_PARIS, provider="opencode-cli")
    
    if "Louvre" in stdout and ("croissant" in stdout.lower() or "bakery" in stdout.lower() or "boulangerie" in stdout.lower()):
        print("✅ SUCCESS: Itinerary contains expected keywords.")
    else:
        print("❌ FAILURE: Itinerary is missing key components.")
        # print(f"STDOUT: {stdout}")
        # print(f"STDERR: {stderr}")

def test_fastapi_app():
    print("\n--- Scenario 2: FastAPI Todo App ---")
    # Clean up old test dir
    subprocess.run(["rm", "-rf", "todo_app"])
    
    message = "Build a FastAPI application that hosts a simple todo list app with CRUD operations. Save the code into a directory named 'todo_app' with a main.py file."
    # Use interactive mode to drive the tool loop
    # Input 'A' for Always Approve tools, then 'exit'
    interactive_input = f"{message}\nA\nexit\n"
    stdout, stderr = run_zeroclaw(None, SESSION_FASTAPI, provider="opencode-cli", interactive_input=interactive_input)
    
    main_py = Path("todo_app/main.py")
    if main_py.exists():
        content = main_py.read_text()
        if "FastAPI" in content and ("router" in content or "app." in content or "todos" in content):
            print("✅ SUCCESS: FastAPI application created with valid-looking code.")
        else:
            print("❌ FAILURE: todo_app/main.py exists but content is unexpected.")
            print(f"Content: {content[:200]}...")
    else:
        print(f"❌ FAILURE: todo_app/main.py was not created.")
        # print(f"STDOUT: {stdout}")
        # print(f"STDERR: {stderr}")

def main():
    if not Path(BINARY).exists():
        print(f"Error: Binary {BINARY} not found. Run 'bazel build //:zeroclaw' first.")
        return

    start_time = time.time()
    
    # Run tests
    test_paris_itinerary()
    test_fastapi_app()
    
    end_time = time.time()
    print(f"\nBenchmark completed in {end_time - start_time:.2f} seconds.")

if __name__ == "__main__":
    main()
