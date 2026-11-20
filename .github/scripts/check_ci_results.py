#!/usr/bin/env python3

"""Fail a terminal CI job unless every serialized dependency succeeded.

Parent workflows pass GitHub's `toJSON(needs)` object through the NEEDS
environment variable. Treat skipped and cancelled dependencies as failures too:
for a required fan-in job, only an explicit success is safe to accept.
"""

import json
import os


def main() -> None:
    # Keep result policy in one script so blocking-ci and postmerge-ci cannot
    # drift in how they interpret dependency conclusions.
    needs = json.loads(os.environ["NEEDS"])
    # Postmerge-ci continues to require all dependencies by default. The PR
    # workflow selects the applicable upstream or fork checks explicitly.
    selected = os.environ.get("REQUIRED_CI_DEPENDENCIES")
    if selected is None:
        required = list(needs)
    else:
        required = [name.strip() for name in selected.split(",")]
        if not all(required) or len(required) != len(set(required)):
            raise SystemExit("Invalid REQUIRED_CI_DEPENDENCIES configuration")

    failures = sorted(
        (name, needs[name]["result"] if name in needs else "missing")
        for name in required
        if name not in needs or needs[name]["result"] != "success"
    )

    if failures:
        print("CI dependencies did not succeed:")
        for name, result in failures:
            print(f"{name}: {result}")
        raise SystemExit(1)

    print("All CI dependencies succeeded.")


if __name__ == "__main__":
    main()
