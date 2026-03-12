#!/usr/bin/env python3
"""
Test message generator for Telegram integration testing.
Generates messages of various lengths for testing message splitting.
"""

import sys

def generate_short_message():
    """Generate a short message (< 100 chars)"""
    return "Hello! This is a short test message."

def generate_medium_message():
    """Generate a medium message (~ 1000 chars)"""
    return "This is a medium-length test message. " * 25

def generate_long_message():
    """Generate a long message (~ 5000 chars, > 4096 limit)"""
    return "This is a very long test message that will be split into multiple chunks. " * 70

def generate_exact_limit_message():
    """Generate a message exactly at 4096 char limit"""
    base = "x" * 4096
    return base

def generate_over_limit_message():
    """Generate a message just over the 4096 char limit"""
    return "x" * 4200

def generate_multi_chunk_message():
    """Generate a message that requires 3+ chunks"""
    return "Lorem ipsum dolor sit amet, consectetur adipiscing elit. " * 250

def generate_newline_message():
    """Generate a message with many newlines (tests newline splitting)"""
    return "Line of text\n" * 400

def generate_word_boundary_message():
    """Generate a message with clear word boundaries"""
    return "word " * 1000

def print_message_info(message, name):
    """Print information about a message"""
    print(f"\n{'='*60}")
    print(f"{name}")
    print(f"{'='*60}")
    print(f"Length: {len(message)} characters")
    print(f"Will split: {'Yes' if len(message) > 4096 else 'No'}")
    if len(message) > 4096:
        chunks = (len(message) + 4095) // 4096
        print(f"Estimated chunks: {chunks}")
    print(f"{'='*60}")
    print(message[:200] + "..." if len(message) > 200 else message)
    print(f"{'='*60}\n")

def main():
    if len(sys.argv) > 1:
        test_type = sys.argv[1].lower()
    else:
        print("Usage: python3 generate_test_messages.py [type]")
        print("\nAvailable types:")
        print("  short      - Short message (< 100 chars)")
        print("  medium     - Medium message (~1000 chars)")
        print("  long       - Long message (~5000 chars, requires splitting)")
        print("  exact      - Exactly 4096 chars")
        print("  over       - Just over 4096 chars")
        print("  multi      - Very long (3+ chunks)")
        print("  newline    - Many newlines (tests line splitting)")
        print("  word       - Clear word boundaries")
        print("  all        - Show info for all types")
        print("\nExample:")
        print("  python3 generate_test_messages.py long")
        sys.exit(1)

    messages = {
        'short': ('Short Message', generate_short_message()),
        'medium': ('Medium Message', generate_medium_message()),
        'long': ('Long Message', generate_long_message()),
        'exact': ('Exact Limit (4096)', generate_exact_limit_message()),
        'over': ('Just Over Limit', generate_over_limit_message()),
        'multi': ('Multi-Chunk Message', generate_multi_chunk_message()),
        'newline': ('Newline Test', generate_newline_message()),
        'word': ('Word Boundary Test', generate_word_boundary_message()),
    }

    if test_type == 'all':
        for name, msg in messages.values():
            print_message_info(msg, name)
    elif test_type in messages:
        name, msg = messages[test_type]
        # Just print the message for piping to Telegram
        print(msg)
    else:
        print(f"Error: Unknown type '{test_type}'")
        print("Run without arguments to see available types.")
        sys.exit(1)

if __name__ == '__main__':
    main()
