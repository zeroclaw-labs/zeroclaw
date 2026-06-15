#!/bin/bash
# Generate root redirect for master branch documentation

cat > index.html <<'HTML'
<!doctype html>
<meta charset="utf-8">
<meta http-equiv="refresh" content="0; url=./master/en/">
<link rel="canonical" href="./master/en/">
<title>ZeroClaw Docs</title>
HTML
