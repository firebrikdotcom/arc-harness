#!/usr/bin/env python3
"""Build the self-contained Task Managers prototype page.

Usage: build.py DATA_JSON OUT_HTML [--selftest]

DATA_JSON comes from basecamp_dump.py and holds real Basecamp text, so keep it
and OUT_HTML outside the repository (for example in a scratch directory).
"""
import json
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
STYLES = HERE.parents[2] / "services" / "harness-audit" / "public" / "styles.css"


def main():
    data_path, out_path = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
    page = (HERE / "task_managers.template.html").read_text(encoding="utf-8")
    data = json.dumps(json.loads(data_path.read_text(encoding="utf-8")), ensure_ascii=False).replace("</", "<\\/")
    page = page.replace("/*__DASHBOARD_CSS__*/", STYLES.read_text(encoding="utf-8")).replace("/*__DATA__*/null", data)
    if "--selftest" in sys.argv[3:]:
        page = page.replace("render();\n</script>", "render();\n" + (HERE / "selftest.js").read_text(encoding="utf-8") + "\n</script>")
    out_path.write_text(page, encoding="utf-8")
    print(f"wrote {out_path} ({len(page)} bytes)")


if __name__ == "__main__":
    main()
