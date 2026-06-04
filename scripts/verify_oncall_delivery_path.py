import pathlib
import sys


ROOT = pathlib.Path(__file__).resolve().parents[1]
PROM = ROOT / "monitoring" / "prometheus.yml"
ALERTS = ROOT / "monitoring" / "alert_rules.yml"
PLAYBOOK = ROOT / "plans" / "oncall_playbook.md"


def read(path: pathlib.Path) -> str:
    return path.read_text(encoding="utf-8")


def parse_alert_names(alert_rules: str):
    names = []
    for raw in alert_rules.splitlines():
        line = raw.strip()
        if line.startswith("- alert:"):
            names.append(line.split(":", 1)[1].strip())
    return names


def main() -> int:
    prom = read(PROM)
    alerts = read(ALERTS)
    playbook = read(PLAYBOOK)

    failures = []

    if "alertmanager:9093" not in prom:
        failures.append("Prometheus alerting target 'alertmanager:9093' is missing")

    required_alerts = [
        "AvailabilityBurnRateFast",
        "AvailabilityBurnRateSlow",
        "LatencyBurnRateFast",
        "LatencyBurnRateSlow",
        "AuthBudgetBurnFast",
        "AuthBudgetBurnSlow",
    ]

    declared_alerts = set(parse_alert_names(alerts))
    for name in required_alerts:
        if name not in declared_alerts:
            failures.append(f"Missing alert rule: {name}")

    for name in required_alerts:
        if f"`{name}`" not in playbook:
            failures.append(f"On-call playbook missing mapped alert reference: {name}")

    if failures:
        for item in failures:
            print(f"ERROR: {item}")
        return 1

    print("OK: alert routing and on-call mapping checks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

