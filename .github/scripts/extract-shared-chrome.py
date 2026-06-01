#!/usr/bin/env python3

import sys
import os
import shutil

def main():
    if len(sys.argv) != 3:
        print("Usage: extract-shared-chrome.py <version_dir> <shared_dir>")
        sys.exit(1)
        
    version_dir = sys.argv[1]
    shared_dir = sys.argv[2]
    
    if not os.path.isdir(version_dir):
        print(f"Error: {version_dir} is not a directory")
        sys.exit(1)
        
    # 1. Find first locale directory
    first_locale = None
    # Usually 'en' or other 2-letter codes. Let's look for 'en' first.
    if os.path.isdir(os.path.join(version_dir, 'en')):
        first_locale = 'en'
    else:
        for entry in os.listdir(version_dir):
            if os.path.isdir(os.path.join(version_dir, entry)) and entry != "api":
                first_locale = entry
                break
                
    if not first_locale:
        print(f"No locales found in {version_dir}")
        sys.exit(0)
        
    src_dir = os.path.join(version_dir, first_locale)
    
    prefixes = [
        "css/chrome",
        "theme/custom",
        "theme/version-selector",
        "theme/lang-switcher",
        "favicon",
    ]
    
    replacements = []
    
    for root, _, files in os.walk(src_dir):
        for file in files:
            full_path = os.path.join(root, file)
            rel_path = os.path.relpath(full_path, src_dir)
            rel_str = rel_path.replace('\\', '/')
            
            if not any(rel_str.startswith(p) for p in prefixes):
                continue
                
            # Strip hash: name-8hex.ext -> name.ext
            pos = file.rfind('-')
            if pos != -1:
                ext_pos = file.rfind('.')
                if ext_pos != -1 and pos < ext_pos:
                    hash_val = file[pos+1:ext_pos]
                    if len(hash_val) == 8 and all(c in '0123456789abcdefABCDEF' for c in hash_val):
                        unhashed_name = file[:pos] + file[ext_pos:]
                        dest_rel = os.path.join(os.path.dirname(rel_path), unhashed_name)
                        dest = os.path.join(shared_dir, dest_rel)
                        
                        os.makedirs(os.path.dirname(dest), exist_ok=True)
                        shutil.copy2(full_path, dest)
                        
                        dest_rel_str = dest_rel.replace('\\', '/')
                        replacements.append((rel_str, f"../../_shared/{dest_rel_str}"))

    # Rewrite HTML in all locales
    for entry in os.listdir(version_dir):
        loc_dir = os.path.join(version_dir, entry)
        if not os.path.isdir(loc_dir) or entry == "api":
            continue
            
        for root, _, files in os.walk(loc_dir):
            for file in files:
                if file.endswith('.html'):
                    full_path = os.path.join(root, file)
                    try:
                        with open(full_path, 'r', encoding='utf-8') as f:
                            content = f.read()
                        
                        changed = False
                        for orig, new in replacements:
                            if orig in content:
                                content = content.replace(orig, new)
                                changed = True
                                
                        if changed:
                            with open(full_path, 'w', encoding='utf-8') as f:
                                f.write(content)
                    except UnicodeDecodeError:
                        pass
                        
                rel_path = os.path.relpath(os.path.join(root, file), loc_dir)
                rel_str = rel_path.replace('\\', '/')
                if any(orig == rel_str for orig, _ in replacements):
                    try:
                        os.remove(os.path.join(root, file))
                    except FileNotFoundError:
                        pass

if __name__ == "__main__":
    main()
