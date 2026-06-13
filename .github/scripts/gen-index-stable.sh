#!/bin/bash
# Generate root redirect for stable documentation

cat > index.html <<'HTML'
<!doctype html>
<meta charset="utf-8">
<meta http-equiv="refresh" content="0; url=./stable/en/">
<link rel="canonical" href="./stable/en/">
<title>ZeroClaw Docs</title>
HTML
