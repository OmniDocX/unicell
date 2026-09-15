#!/usr/bin/env python
# -*- coding: utf-8 -*-
# LaTeX -> OMML(OOXML math) 批量转换（供 UniCell 导出 xlsx 时把 $..$ / $$..$$ 公式
# 转为 Excel 可识别的 OOXML 数学）。实现方法参考母项目 unidoc/tools/udoc3_to_docx.py：
#   latex2mathml -> MathML -> mathml2omml -> OMML (separately installed libraries).
# 用法： python latex2omml.py <in.json> <out.json>
#   in.json:  [{"key":"0_1_1","latex":"\\int_a^b f(x)dx"}, ...]
#   out.json: {"0_1_1": "<m:oMathPara ...>...</m:oMathPara>" 或 null}
import sys
import os
import json

HERE = os.path.dirname(os.path.abspath(__file__))
MATH_NS = "http://schemas.openxmlformats.org/officeDocument/2006/math"


def latex_to_omml(latex):
    import latex2mathml.converter
    import lxml.etree as etree
    mathml = latex2mathml.converter.convert(latex)
    import mathml2omml
    fragment = mathml2omml.convert(mathml)
    wrapper = etree.fromstring((f'<root xmlns:m="{MATH_NS}">' + fragment + '</root>').encode('utf-8'))
    root = wrapper[0]
    # 包为 oMathPara（Excel 形状内公式的标准容器），并清理未用的 mml 命名空间
    if etree.QName(root.tag).localname == "oMath":
        para = etree.Element("{%s}oMathPara" % MATH_NS, nsmap={"m": MATH_NS})
        para.append(root)
        root = para
    etree.cleanup_namespaces(root)
    return etree.tostring(root, encoding="unicode")


def main():
    if len(sys.argv) < 3:
        print("usage: latex2omml.py <in.json> <out.json>", file=sys.stderr)
        sys.exit(2)
    with open(sys.argv[1], "r", encoding="utf-8-sig") as f:
        items = json.load(f)
    out = {}
    for it in items:
        key = it.get("key")
        latex = (it.get("latex") or "").strip()
        try:
            out[key] = latex_to_omml(latex) if latex else None
        except Exception:
            out[key] = None
    with open(sys.argv[2], "w", encoding="utf-8") as f:
        json.dump(out, f, ensure_ascii=False)


if __name__ == "__main__":
    main()
