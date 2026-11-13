//! The small local helpers smol installs for the agent: web search and page
//! reading that run on this machine instead of through an extension like exa.
//!
//! Both are plain Python 3 scripts with no dependencies beyond the standard
//! library. smol writes them next to its config and points the system prompt at
//! them; the model runs them with pi's `bash` tool.

/// `websearch [ -n COUNT ] query...` — search the web from this machine.
pub const WEBSEARCH: &str = r##"#!/usr/bin/env python3
"""Local web search for smol.

Runs on this machine against public engines (or your own SearXNG), so no API
key and no exa-style service is involved.

Usage:
    websearch [-n COUNT] query words...

Config, optional, at ~/.config/smol/search.json:
    {"searxng": "http://localhost:8888"}

Output:
    # engine <name> · <n> results for: <query>
    1. Title
       https://real.url
       snippet
"""
import base64
import html
import json
import os
import re
import sys
import urllib.parse
import urllib.request

UA = ("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 "
      "(KHTML, like Gecko) Chrome/120.0 Safari/537.36")
TIMEOUT = 15
MAX_BYTES = 2_000_000


def config_path():
    base = os.environ.get("XDG_CONFIG_HOME") or os.path.join(
        os.path.expanduser("~"), ".config")
    return os.path.join(base, "smol", "search.json")


def load_config():
    try:
        with open(config_path(), encoding="utf-8") as handle:
            data = json.load(handle)
        return data if isinstance(data, dict) else {}
    except Exception:
        return {}


def fetch(url, data=None):
    request = urllib.request.Request(
        url,
        data=data,
        headers={"User-Agent": UA, "Accept": "text/html,application/json"},
    )
    with urllib.request.urlopen(request, timeout=TIMEOUT) as response:
        return response.read(MAX_BYTES).decode("utf-8", "replace")


def strip_tags(text):
    text = re.sub(r"<[^>]+>", " ", text)
    return re.sub(r"\s+", " ", html.unescape(text)).strip()


def unwrap(url):
    """Bing hides the real link behind /ck/a?...&u=a1<base64url>."""
    if "bing.com/ck/a" not in url:
        return url
    try:
        params = urllib.parse.parse_qs(urllib.parse.urlparse(url).query)
        value = params.get("u", [""])[0]
        if value.startswith("a1"):
            value = value[2:]
        value += "=" * (-len(value) % 4)
        return base64.urlsafe_b64decode(value).decode("utf-8", "replace")
    except Exception:
        return url


def parse_bing(page):
    results = []
    for block in re.findall(r'<li class="b_algo".*?</li>', page, re.S):
        link = re.search(r'<h2>\s*<a[^>]*href="([^"]+)"[^>]*>(.*?)</a>',
                         block, re.S)
        if not link:
            continue
        url = unwrap(html.unescape(link.group(1)))
        title = strip_tags(link.group(2))
        snippet = re.search(r"<p[^>]*>(.*?)</p>", block, re.S)
        if title and url.startswith("http"):
            results.append(
                (title, url, strip_tags(snippet.group(1)) if snippet else ""))
    return results


def parse_duckduckgo(page):
    results = []
    for link in re.finditer(r'<a[^>]+href="([^"]+)"[^>]*>(.*?)</a>', page,
                            re.S):
        href = html.unescape(link.group(1))
        title = strip_tags(link.group(2))
        if "uddg=" in href:
            if href.startswith("//"):
                href = "https:" + href
            href = urllib.parse.parse_qs(
                urllib.parse.urlparse(href).query).get("uddg", [""])[0]
        elif href.startswith("//"):
            href = "https:" + href
        if href.startswith("http") and len(title) > 4:
            results.append((title, href, ""))
    return results


def parse_mojeek(page):
    results = []
    for block in re.findall(r"<h2><a href=\"(https?://[^\"]+)\"[^>]*>(.*?)"
                            r"</a></h2>(.*?)</li>", page, re.S):
        url, title, rest = block
        snippet = re.search(r"<p[^>]*>(.*?)</p>", rest, re.S)
        results.append((strip_tags(title), html.unescape(url),
                        strip_tags(snippet.group(1)) if snippet else ""))
    return results


def parse_searx(page):
    data = json.loads(page)
    return [
        (item.get("title") or item.get("url", ""), item.get("url", ""),
         (item.get("content") or "").strip())
        for item in data.get("results", [])
        if item.get("url")
    ]


def engines(config):
    listed = []
    base = str(config.get("searxng") or "").rstrip("/")
    if base:
        listed.append((
            "searxng",
            lambda query: base + "/search?" + urllib.parse.urlencode(
                {"q": query, "format": "json"}),
            parse_searx,
        ))
    listed.append((
        "bing",
        lambda query: "https://www.bing.com/search?" + urllib.parse.urlencode(
            {"q": query, "count": "20", "setlang": "en"}),
        parse_bing,
    ))
    listed.append((
        "duckduckgo",
        lambda query: "https://lite.duckduckgo.com/lite/?" +
        urllib.parse.urlencode({"q": query}),
        parse_duckduckgo,
    ))
    listed.append((
        "mojeek",
        lambda query: "https://www.mojeek.com/search?" +
        urllib.parse.urlencode({"q": query}),
        parse_mojeek,
    ))
    return listed


def parse_args(argv):
    count = 8
    index = 0
    while index < len(argv):
        arg = argv[index]
        if arg in ("-n", "--num", "--count"):
            if index + 1 >= len(argv):
                raise ValueError("{0} needs a number".format(arg))
            count = int(argv[index + 1])
            index += 2
            continue
        if arg.startswith("-n") and arg[2:].isdigit():
            count = int(arg[2:])
            index += 1
            continue
        break
    return count, " ".join(argv[index:]).strip()


def main(argv):
    # Failures must reach the model in the same order as the page header.
    try:
        sys.stdout.reconfigure(line_buffering=True)
    except Exception:
        pass
    try:
        count, query = parse_args(argv)
    except ValueError as error:
        print("websearch: {0}".format(error), file=sys.stderr)
        return 2
    if not query:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    count = max(1, min(count, 20))

    failures = []
    for name, build, parse in engines(load_config()):
        try:
            results = parse(fetch(build(query)))
        except Exception as error:
            failures.append("{0}: {1}".format(name, error))
            continue
        if not results:
            failures.append("{0}: no results".format(name))
            continue
        print("# engine {0} · {1} results for: {2}".format(
            name, min(count, len(results)), query))
        for number, (title, url, snippet) in enumerate(results[:count], 1):
            print("{0}. {1}".format(number, title))
            print("   {0}".format(url))
            if snippet:
                print("   {0}".format(snippet))
        return 0

    print("search failed (no engine answered) — " + "; ".join(failures))
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
"##;

/// `webfetch [ -n CHARS ] url...` — read a page as plain text.
pub const WEBFETCH: &str = r##"#!/usr/bin/env python3
"""Read a web page as plain text for the agent.

Usage:
    webfetch [-n CHARS] url...

Defaults to 4000 characters per page (max 50000). A missing scheme becomes
https://. HTML is stripped of script/style/navigation noise and wrapped at
about 100 columns.
"""
import re
import sys
import textwrap
import urllib.request

UA = ("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 "
      "(KHTML, like Gecko) Chrome/120.0 Safari/537.36")
TIMEOUT = 20
MAX_BYTES = 4_000_000
DEFAULT_CHARS = 4000
MAX_CHARS = 50000


def fetch(url):
    request = urllib.request.Request(
        url, headers={"User-Agent": UA, "Accept": "text/html,text/plain,*/*"})
    with urllib.request.urlopen(request, timeout=TIMEOUT) as response:
        raw = response.read(MAX_BYTES)
        content_type = response.headers.get("Content-Type", "")
    return raw, content_type


def to_text(raw, content_type):
    charset = "utf-8"
    match = re.search(r"charset=([\w-]+)", content_type or "")
    if match:
        charset = match.group(1)
    text = raw.decode(charset, "replace")
    if "html" not in (content_type or "") and "<html" not in text[:2000].lower():
        return text
    text = re.sub(r"(?is)<(script|style|noscript|svg|nav|header|footer|aside)"
                  r"\b.*?</\1>", " ", text)
    text = re.sub(r"(?is)<!--.*?-->", " ", text)
    text = re.sub(r"(?i)<(br|/p|/div|/li|/tr|/h[1-6])\s*/?>", "\n", text)
    text = re.sub(r"(?s)<[^>]+>", " ", text)
    return re.sub(r"[ \t]+", " ", text).replace("\xa0", " ")


def normalize(url):
    if re.match(r"^[a-z][a-z0-9+.-]*://", url, re.I):
        return url
    return "https://" + url


def read_page(url, limit):
    raw, content_type = fetch(normalize(url))
    text = to_text(raw, content_type)
    lines = []
    for line in text.splitlines():
        line = line.strip()
        if line:
            lines.append(line)
    body = "\n".join(lines)
    truncated = len(body) > limit
    body = body[:limit]
    out = textwrap.fill(body, width=100, replace_whitespace=False,
                        drop_whitespace=True) if "\n" not in body[:200] else body
    if truncated:
        out += "\n... [truncated, fetch again with -n for more]"
    return out


def parse_args(argv):
    limit = DEFAULT_CHARS
    index = 0
    while index < len(argv):
        arg = argv[index]
        if arg in ("-n", "--chars"):
            if index + 1 >= len(argv):
                raise ValueError("{0} needs a number".format(arg))
            limit = int(argv[index + 1])
            index += 2
            continue
        if arg.startswith("-n") and arg[2:].isdigit():
            limit = int(arg[2:])
            index += 1
            continue
        break
    return limit, argv[index:]


def main(argv):
    try:
        sys.stdout.reconfigure(line_buffering=True)
    except Exception:
        pass
    try:
        limit, urls = parse_args(argv)
    except ValueError as error:
        print("webfetch: {0}".format(error), file=sys.stderr)
        return 2
    if not urls:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    limit = max(200, min(limit, MAX_CHARS))

    status = 0
    for number, url in enumerate(urls):
        if number:
            print()
        target = normalize(url)
        print("=== {0}".format(target))
        try:
            print(read_page(url, limit))
        except Exception as error:
            # On stdout, so whoever reads the result sees why the page is empty.
            print("FAILED: {0}".format(error))
            status = 1
    return status


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
"##;
