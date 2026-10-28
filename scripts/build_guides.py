import html
import subprocess
import tempfile
from pathlib import Path

from guides_data import GUIDES

OUT = Path(__file__).resolve().parent.parent / "docs" / "gateway-guide.pdf"

CSS = """
@page { size: 1112px 635px; margin: 0; }
* { box-sizing: border-box; }
html, body { margin: 0; background: #fff; -webkit-print-color-adjust: exact; print-color-adjust: exact; }
body { color: #243746; font-family: "Noto Sans CJK TC", "Noto Sans TC", sans-serif; }
.page { width: 1112px; height: 635px; padding: 22px 34px 34px; page-break-after: always; overflow: hidden; position: relative; background: #fff; }
.page:last-child { page-break-after: auto; }
h1 { margin: 0 0 4px; text-align: center; font-size: 21px; color: #193649; letter-spacing: .03em; }
.lead { text-align: center; font-size: 14px; color: #4b6573; margin: 0 0 12px; }
.badge { display: inline-block; font-size: 10px; font-weight: 700; padding: 2px 9px; border-radius: 10px; background: #e7f1f7; color: #26729a; margin-left: 8px; }
.sec { font-size: 13px; font-weight: 800; margin: 0 0 6px; padding: 3px 8px; border-left: 5px solid #43829f; background: #edf4f7; }
.box { border: 1.5px solid #92a8b4; border-radius: 7px; background: #fff; text-align: center; padding: 7px 6px; }
.box b { display: block; font-size: 15px; color: #193649; margin-bottom: 3px; }
.box span { display: block; font-size: 12px; line-height: 1.45; color: #44575c; }
.data { background: #eef7f0; border-color: #83ad8b; }
.control { background: #e7f1f7; border-color: #8eb5c9; }
.obs { background: #f5edfa; border-color: #ad8fc0; }
.infra { background: #fff3e0; border-color: #d6a34e; }
.here { background: #d9efe0; border: 2.5px solid #2f8a5b; }
.chain { display: grid; grid-template-columns: 1fr 120px 1.3fr 120px 1fr; align-items: center; margin-top:6px; margin-bottom: 22px; }
.ar { position: relative; height: 2px; background: #1f7a70; margin: 0 6px; }
.ar::after { content: ""; position: absolute; right: -2px; top: -6px; border: 7px solid transparent; border-left: 12px solid #1f7a70; border-right: 0; }
.ar i { position: absolute; left: -30px; right: -30px; top: -17px; text-align: center; font-style: normal; font-size: 11px; color: #1f7a70; white-space: nowrap; }
.flow { display: grid; align-items: stretch; margin-bottom: 8px; }
.flow .st { display: flex; flex-direction: column; justify-content: center; min-height: 110px; }
.flow .gap { display: flex; align-items: center; }
.flow .gap .ar { width: 100%; margin: 0 2px; }
.vwrap { display: flex; justify-content: center; height: 56px; }
.v { position: relative; width: 2px; background: #6b8a8f; }
.v::after { content: ""; position: absolute; bottom: -2px; left: -6px; border: 7px solid transparent; border-top: 12px solid #6b8a8f; border-bottom: 0; }
.v i { position: absolute; left: 10px; top: 14px; font-style: normal; font-size: 11px; color: #526d7b; white-space: nowrap; }
.group { border: 1.5px dashed #8eb5c9; border-radius: 10px; padding: 14px 10px 10px; position: relative; margin-bottom: 8px; }
.group > .gt { position: absolute; top: -9px; left: 12px; background: #fff; padding: 0 6px; font-size: 10px; font-weight: 800; color: #526d7b; }
table { width: 100%; border-collapse: collapse; table-layout: fixed; font-size: 11.5px; line-height: 1.45; }
th { text-align: left; color: #496572; border-bottom: 1px solid #91a7b1; padding: 4px 6px; }
td { vertical-align: top; border-bottom: 1px solid #dde5e8; padding: 4px 6px; }
td:first-child { font-weight: 700; color: #1d485f; width: 120px; }
.cols { display: grid; grid-template-columns: 1fr 1fr; gap: 14px; }
.footer { position: absolute; bottom: 9px; left: 34px; right: 34px; display: flex; justify-content: space-between; color: #718692; font-size: 8px; border-top: 1px solid #c8d4da; padding-top: 5px; }
.note { font-size: 12.5px; color: #526d7b; margin-top: 6px; line-height: 1.5; }
"""


def e(text):
    return html.escape(text).replace("\n", "<br>")


def box(item, cls="data"):
    return f'<div class="box {cls}"><b>{e(item[0])}</b><span>{e(item[1])}</span></div>'


def chain(g):
    prev, nxt = g["prev"], g["next"]
    return (
        '<div class="chain">'
        + box(prev[:2], "control")
        + f'<div class="ar"><i>{e(prev[2])}</i></div>'
        + f'<div class="box here"><b>{e(g["short"])}</b></div>'
        + f'<div class="ar"><i>{e(nxt[2])}</i></div>'
        + box(nxt[:2], "infra")
        + "</div>"
    )


def flow(steps, stores=None, labels=None, cls="data"):
    n = len(steps)
    gw = {1: 110, 2: 110, 3: 110, 4: 96, 5: 80, 6: 70, 7: 56}.get(n, 36)
    cols = " ".join("1fr" if i % 2 == 0 else f"{gw}px" for i in range(2 * n - 1))
    cells = []
    for i, s in enumerate(steps):
        cells.append(f'<div class="st">{box(s, cls)}</div>')
        if i < n - 1:
            lab = labels[i] if labels and labels[i] else ""
            cells.append(f'<div class="gap"><div class="ar"><i>{e(lab)}</i></div></div>')
    out = f'<div class="flow" style="grid-template-columns:{cols}">' + "".join(cells)
    if stores:
        by_idx = {s[2]: s for s in stores}
        for i in range(n):
            if i in by_idx:
                out += f'<div class="vwrap"><div class="v"><i>{e(by_idx[i][3])}</i></div></div>'
            else:
                out += "<div></div>"
            if i < n - 1:
                out += "<div></div>"
        for i in range(n):
            if i in by_idx:
                out += f'<div>{box(by_idx[i][:2], "obs")}</div>'
            else:
                out += "<div></div>"
            if i < n - 1:
                out += "<div></div>"
    return out + "</div>"


def table(rows, head=("項目", "說明")):
    body = "".join(f"<tr><td>{e(a)}</td><td>{e(b)}</td></tr>" for a, b in rows)
    return f"<table><tr><th>{head[0]}</th><th>{head[1]}</th></tr>{body}</table>"


def page(inner, g, n, total):
    return (
        f'<section class="page">{inner}'
        f'<div class="footer"><span>{e(g["source"])}</span><span>{n}/{total}</span></div></section>'
    )


def build(g):
    total = 2
    p1 = (
        f'<h1>{e(g["title"])}<span class="badge">{e(g["status"])}</span></h1>'
        f'<p class="lead">{e(g["lead"])}</p>'
        + '<div class="sec">它在整體裡的位置</div>'
        + chain(g)
        + f'<div class="sec">{e(g["flow_title"])}</div>'
        + flow(g["steps"], g.get("stores"), g.get("labels"))
        + (f'<p class="note">{e(g["note"])}</p>' if g.get("note") else "")
    )
    p2 = '<h1>細節對照</h1>'
    if g.get("flow2"):
        f2 = g["flow2"]
        p2 += f'<div class="sec">{e(f2["title"])}</div>' + flow(f2["steps"], None, f2.get("labels"), "control") + '<div style="height:10px"></div>'
    p2 += '<div class="cols"><div>'
    p2 += '<div class="sec">做什麼</div>' + table(g["does"], ("項目", "說明"))
    p2 += '</div><div>'
    p2 += '<div class="sec">出問題時會怎樣</div>' + table(g["fails"], ("狀況", "結果"))
    p2 += '<div style="height:10px"></div><div class="sec">不做什麼與限制</div>' + table(g["limits"], ("項目", "說明"))
    p2 += "</div></div>"
    return (
        "<!doctype html><html lang='zh-Hant'><head><meta charset='utf-8'><title>"
        + e(g["title"])
        + "</title><style>"
        + CSS
        + "</style></head><body>"
        + page(p1, g, 1, total)
        + page(p2, g, 2, total)
        + "</body></html>"
    )


def render(html_path, pdf_path):
    subprocess.run(
        [
            "google-chrome",
            "--headless=new",
            "--disable-gpu",
            "--no-sandbox",
            "--no-pdf-header-footer",
            f"--print-to-pdf={pdf_path}",
            f"file://{html_path}",
        ],
        check=True,
        capture_output=True,
    )


def main():
    parts = []
    with tempfile.TemporaryDirectory() as tmp:
        for g in GUIDES:
            h = Path(tmp) / f'{g["id"]}.html'
            h.write_text(build(g), encoding="utf-8")
            pdf = Path(tmp) / f'{g["id"]}.pdf'
            render(h, pdf)
            parts.append(str(pdf))
        subprocess.run(["pdfunite", *parts, str(OUT)], check=True)
    print(OUT)


if __name__ == "__main__":
    main()
