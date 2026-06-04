import json
import pathlib
import time


ROOT = pathlib.Path(__file__).resolve().parents[1]
ART = ROOT / "plans" / "load_test_artifacts"


def exists(path: pathlib.Path) -> bool:
    return path.exists() and path.is_file()


def main() -> int:
    files = {
        "nft_summary": ART / "latest_summary.md",
        "nft_single": ART / "latest_summary_single.md",
        "nft_scaled": ART / "latest_summary_scaled.md",
        "single_results": ART / "single_replica_results.json",
        "scaled_results": ART / "scaled_replica_results.json",
        "shared_state_validation": ART / "shared_state_validation.json",
        "trend_history": ART / "trend_history.csv",
        "rollback_rehearsal_evidence": ROOT / "rollback-rehearsal-evidence.txt",
    }

    status = {name: exists(path) for name, path in files.items()}
    missing = [name for name, ok in status.items() if not ok]

    payload = {
        "timestamp_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "release_policy_ref": "RELEASE_POLICY.md",
        "evidence_status": status,
        "missing": missing,
        "result": "PASS" if not missing else "WARN",
    }

    json_out = ART / "release_evidence_bundle.json"
    md_out = ART / "release_evidence_bundle.md"

    json_out.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")

    lines = [
        "# Release Evidence Bundle",
        "",
        f"- **Timestamp (UTC):** {payload['timestamp_utc']}",
        f"- **Policy reference:** `{payload['release_policy_ref']}`",
        "",
        "| Evidence item | Present | Path |",
        "|---|---|---|",
    ]

    for name, path in files.items():
        present = "✅" if status[name] else "❌"
        lines.append(f"| {name} | {present} | `{path.relative_to(ROOT)}` |")

    lines.append("")
    lines.append(f"- Result: **{payload['result']}**")
    if missing:
        lines.append(f"- Missing: {', '.join(missing)}")

    md_out.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"wrote {json_out}")
    print(f"wrote {md_out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

