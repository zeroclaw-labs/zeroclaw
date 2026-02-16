"""
Web-related tools: HTTP requests and web search.
"""

import json
import os
import urllib.error
import urllib.parse
import urllib.request

from langchain_core.tools import tool


@tool
def http_request(url: str, method: str = "GET", headers: str = "", body: str = "") -> str:
    """
    Make an HTTP request to a URL.

    Args:
        url: The URL to request
        method: HTTP method (GET, POST, PUT, DELETE, etc.)
        headers: Comma-separated headers in format "Name: Value, Name2: Value2"
        body: Request body for POST/PUT requests

    Returns:
        The response status and body
    """
    try:
        req_headers = {"User-Agent": "ZeroClaw/1.0"}
        if headers:
            for h in headers.split(","):
                if ":" in h:
                    k, v = h.split(":", 1)
                    req_headers[k.strip()] = v.strip()

        data = body.encode() if body else None
        req = urllib.request.Request(url, data=data, headers=req_headers, method=method.upper())

        with urllib.request.urlopen(req, timeout=30) as resp:
            body_text = resp.read().decode("utf-8", errors="replace")
            return f"Status: {resp.status}\n{body_text[:5000]}"
    except urllib.error.HTTPError as e:
        error_body = e.read().decode("utf-8", errors="replace")[:1000]
        return f"HTTP Error {e.code}: {error_body}"
    except Exception as e:
        return f"Error: {e}"


@tool
def web_search(query: str) -> str:
    """
    Search the web using Brave Search API.

    Requires BRAVE_API_KEY environment variable to be set.

    Args:
        query: The search query

    Returns:
        Search results as formatted text
    """
    api_key = os.environ.get("BRAVE_API_KEY", "")
    if not api_key:
        return "Error: BRAVE_API_KEY environment variable not set. Get one at https://brave.com/search/api/"

    try:
        encoded_query = urllib.parse.quote(query)
        url = f"https://api.search.brave.com/res/v1/web/search?q={encoded_query}"

        req = urllib.request.Request(
            url, headers={"Accept": "application/json", "X-Subscription-Token": api_key}
        )

        with urllib.request.urlopen(req, timeout=10) as resp:
            data = json.loads(resp.read().decode())
            results = []

            for item in data.get("web", {}).get("results", [])[:5]:
                title = item.get("title", "No title")
                url_link = item.get("url", "")
                desc = item.get("description", "")[:200]
                results.append(f"- {title}\n  {url_link}\n  {desc}")

            if not results:
                return "No results found"
            return "\n\n".join(results)
    except Exception as e:
        return f"Error: {e}"
