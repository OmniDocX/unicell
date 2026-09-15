# Product comparison

[简体中文](COMPARISON.zh-CN.md) · [Project overview](../README.md)

## Reference products and scope

**Microsoft 365 (Office 365) and WPS Office are the reference office products for OmniDoc.** UniPPT addresses presentation workflows; UniCell addresses spreadsheet workflows. vecmeta is an underlying vector conversion library and CLI, rather than an office suite.

The project's ambition is to build the most complete China-developed office platform with publicly available source. This is a development objective, not an independently established market ranking. The current public release covers the components listed below; it does not include a complete replacement for every Microsoft 365 or WPS application.

## Capability matrix

Reviewed on 2026-09-16. Competitor columns summarize official product documentation, not hands-on compatibility or performance tests. Availability varies by edition, platform, region and subscription.

| Dimension | OmniDoc public edition | Microsoft 365 (Office 365) | WPS Office |
| --- | --- | --- | --- |
| Presentation workflow | UniPPT: browser editor, native PPTX objects, local import/export | PowerPoint desktop and web applications | WPS presentation applications |
| Spreadsheet workflow | UniCell: browser editor, formulas, XLSX/CSV import/export | Excel desktop and web applications | WPS spreadsheet applications |
| Product breadth | Two local editors plus a vector conversion component in this collection | Broader productivity application and service portfolio | Integrated document, spreadsheet, presentation and PDF tools |
| Distribution | Buildable Rust/JavaScript source, local services and CLI | Official application and service distribution | Official application downloads and online products |
| Cloud collaboration | Not included in the public local edition | Cloud file access and collaboration via Microsoft 365 services | Cloud document collaboration in the WPS ecosystem |
| Source and modification | Source supplied under the repository license; commercial uses outside its permitted purposes require a separate paid written license | Governed by Microsoft's product terms | Governed by WPS product terms |
| Extensibility in this release | Local MCP in UniPPT/UniCell; Rust APIs and CLI in vecmeta | Outside this comparison's verified scope | Outside this comparison's verified scope |
| Performance evidence | Reproducible local benchmarks and raw observations in each repository | Not measured in this benchmark campaign | Not measured in this benchmark campaign |

Sources: [Microsoft applications and services](https://www.microsoft.com/en-us/microsoft-365/products-apps-services), [Microsoft collaboration documentation](https://support.microsoft.com/en-us/office/collab-files/collaborate-from-anywhere-using-microsoft-365), [WPS Office product page](https://www.wps.com/office/), [WPS company and collaboration overview](https://www.wps.com/blog/what-is-kingsoft/).

## Interpreting the benchmarks

Published measurements characterize the included release binaries on one machine. They do not establish a speed advantage over Microsoft 365 or WPS Office. File generation, formula calculation, rendering and interactive editing are different operations and must be reported separately. vecmeta's CLI timings cannot be compared directly with an office application's document-open time.

For a controlled comparison, use identical authorized fixtures and hardware; record application version, edition, architecture, calculation settings, fonts, file hashes and warm/cold state. Time file-open to calculation completion, edits to recalculation completion, and save to completed file write separately. Check output values, object structure and rendering before comparing latency. Report failed cases, all observations, median and P95; keep launch time and network-dependent AI/cloud work separate. No comparative speed ratio should be published until both products have been measured under that protocol.

## Licensing terminology

Current OmniDoc first-party code uses the PolyForm Noncommercial 1.0.0. It is **source-available, not OSI-approved open source**. Third-party packages retain their own licenses. References to source publication or project ambitions do not alter these terms.

Microsoft 365, Office, PowerPoint, Excel and WPS are names or trademarks of their respective owners. This is an independent project comparison.
