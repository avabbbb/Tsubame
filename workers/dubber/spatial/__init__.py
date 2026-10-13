"""Tsubame ``audio.spatialize`` worker (spatial follow), protocol v1.

Portions adapted from ASMR-Dubber (MIT), Copyright (c) 2026 ASMR Dubber
contributors, https://github.com/EveningStudy/asmr-dubber commit 2faea94f.
See ``README.md`` in this directory and ``NOTICE.md`` at the repository root.
"""

WORKER_NAME = "tsubame-spatial"
WORKER_VERSION = "0.1.0"
# Bump when analysis output for the same input/params can change.
ANALYZER_VERSION = "spatial-analyze/1"
# Bump when rendered samples for the same inputs can change.
RENDERER_VERSION = "spatial-render/1"
