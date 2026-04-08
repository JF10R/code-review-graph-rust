#!/usr/bin/env python3
"""Generate markdown summary tables from eval/cases.json and eval/results.json.

Usage:
    python eval/eval-report.py                    # full summary
    python eval/eval-report.py --case httpx-002   # single case
    python eval/eval-report.py --variant scout_mcp # single variant
    python eval/eval-report.py --round R9-rerun   # single round
"""
import json
import argparse
from pathlib import Path
from collections import defaultdict

EVAL_DIR = Path(__file__).parent
CASES_FILE = EVAL_DIR / "cases.json"
RESULTS_FILE = EVAL_DIR / "results.json"


def load_data():
    with open(CASES_FILE) as f:
        cases = json.load(f)["cases"]
    with open(RESULTS_FILE) as f:
        results = json.load(f)["runs"]
    return cases, results


def fmt_time(s):
    if s >= 60:
        return f"{s // 60}m{s % 60:02d}s"
    return f"{s}s"


def case_summary(cases, results, case_id):
    case = cases.get(case_id)
    if not case:
        return f"Unknown case: {case_id}"
    runs = [r for r in results if r["case"] == case_id]
    if not runs:
        return f"No results for {case_id}"

    lines = [
        f"## {case_id}",
        f"**Repo**: [{case['repo']}]({case.get('issue', '')})",
        f"**Prompt**: {case['prompt']}",
        f"**Ground truth**: `{case['ground_truth']['primary_file']}`",
        f"**Difficulty**: {case['difficulty']} | **Language**: {case['language']} | **Files**: {case['repo_size_files']}",
        "",
        "| Variant | Model | Date | Time | Tokens | Tools | RC | Secondary | Notes |",
        "|---------|-------|------|------|--------|-------|----|-----------|-------|",
    ]
    for r in sorted(runs, key=lambda x: (x["date"], x["time_s"])):
        rc = "YES" if r.get("root_cause_found") else "NO"
        sec = r.get("secondary_score", "—")
        notes = r.get("notes", "")
        lines.append(
            f"| {r['variant']} | {r['model']} | {r['date']} "
            f"| {fmt_time(r['time_s'])} | {r['tokens_k']}K | {r['tool_calls']}t "
            f"| {rc} | {sec} | {notes} |"
        )
    return "\n".join(lines)


def variant_summary(cases, results, variant):
    runs = [r for r in results if r["variant"] == variant]
    if not runs:
        return f"No results for variant: {variant}"

    lines = [
        f"## Variant: {variant}",
        "",
        "| Case | Model | Date | Time | Tokens | Tools | RC | Secondary |",
        "|------|-------|------|------|--------|-------|----|-----------|",
    ]
    for r in sorted(runs, key=lambda x: (x["case"], x["date"])):
        rc = "YES" if r.get("root_cause_found") else "NO"
        sec = r.get("secondary_score", "—")
        lines.append(
            f"| {r['case']} | {r['model']} | {r['date']} "
            f"| {fmt_time(r['time_s'])} | {r['tokens_k']}K | {r['tool_calls']}t "
            f"| {rc} | {sec} |"
        )

    # averages
    avg_time = sum(r["time_s"] for r in runs) / len(runs)
    avg_tokens = sum(r["tokens_k"] for r in runs) / len(runs)
    avg_tools = sum(r["tool_calls"] for r in runs) / len(runs)
    rc_rate = sum(1 for r in runs if r.get("root_cause_found")) / len(runs) * 100
    lines.append(f"| **Average** | | | **{fmt_time(int(avg_time))}** | **{avg_tokens:.0f}K** | **{avg_tools:.0f}t** | **{rc_rate:.0f}%** | |")
    return "\n".join(lines)


def round_summary(cases, results, round_tag):
    runs = [r for r in results if r.get("round") == round_tag]
    if not runs:
        return f"No results for round: {round_tag}"

    lines = [
        f"## Round: {round_tag}",
        "",
        "| Case | Variant | Model | Time | Tokens | Tools | RC | Secondary |",
        "|------|---------|-------|------|--------|-------|----|-----------|",
    ]
    for r in sorted(runs, key=lambda x: (x["case"], x["time_s"])):
        rc = "YES" if r.get("root_cause_found") else "NO"
        sec = r.get("secondary_score", "—")
        lines.append(
            f"| {r['case']} | {r['variant']} | {r['model']} "
            f"| {fmt_time(r['time_s'])} | {r['tokens_k']}K | {r['tool_calls']}t "
            f"| {rc} | {sec} |"
        )
    return "\n".join(lines)


def full_summary(cases, results):
    lines = ["# Eval Report", ""]

    # overview
    case_ids = sorted(set(r["case"] for r in results))
    variants = sorted(set(r["variant"] for r in results))
    lines.append(f"**Cases**: {len(case_ids)} | **Variants**: {len(variants)} | **Total runs**: {len(results)}")
    lines.append("")

    # latest results per case (most recent date, fastest variant)
    lines.append("## Latest Results by Case")
    lines.append("")
    lines.append("| Case | Difficulty | Best Variant | Time | Tokens | Tools | RC |")
    lines.append("|------|-----------|-------------|------|--------|-------|----|")
    for cid in case_ids:
        case = cases.get(cid, {})
        runs = [r for r in results if r["case"] == cid]
        latest_date = max(r["date"] for r in runs)
        latest_runs = [r for r in runs if r["date"] == latest_date]
        best = min(latest_runs, key=lambda x: x["time_s"])
        rc = "YES" if best.get("root_cause_found") else "NO"
        diff = case.get("difficulty", "?")
        lines.append(
            f"| {cid} | {diff} | {best['variant']} ({best['model']}) "
            f"| {fmt_time(best['time_s'])} | {best['tokens_k']}K | {best['tool_calls']}t | {rc} |"
        )
    lines.append("")

    # per-variant averages
    lines.append("## Variant Averages (all runs)")
    lines.append("")
    lines.append("| Variant | Runs | Avg Time | Avg Tokens | Avg Tools | RC Rate |")
    lines.append("|---------|------|----------|------------|-----------|---------|")
    for v in variants:
        vruns = [r for r in results if r["variant"] == v]
        avg_time = sum(r["time_s"] for r in vruns) / len(vruns)
        avg_tokens = sum(r["tokens_k"] for r in vruns) / len(vruns)
        avg_tools = sum(r["tool_calls"] for r in vruns) / len(vruns)
        rc_rate = sum(1 for r in vruns if r.get("root_cause_found")) / len(vruns) * 100
        lines.append(
            f"| {v} | {len(vruns)} | {fmt_time(int(avg_time))} | {avg_tokens:.0f}K | {avg_tools:.0f}t | {rc_rate:.0f}% |"
        )
    lines.append("")

    # detailed per-case sections
    for cid in case_ids:
        lines.append(case_summary(cases, results, cid))
        lines.append("")

    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description="Generate eval report from JSON data")
    parser.add_argument("--case", help="Show results for a specific case")
    parser.add_argument("--variant", help="Show results for a specific variant")
    parser.add_argument("--round", help="Show results for a specific round")
    args = parser.parse_args()

    cases, results = load_data()

    if args.case:
        print(case_summary(cases, results, args.case))
    elif args.variant:
        print(variant_summary(cases, results, args.variant))
    elif args.round:
        print(round_summary(cases, results, args.round))
    else:
        print(full_summary(cases, results))


if __name__ == "__main__":
    main()
