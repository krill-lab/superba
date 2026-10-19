#!/usr/bin/env python3
"""rsync -e adapter for a helper accepting one remote command argument."""

import os
import shlex
import sys

helper = os.environ["SUPERBA_MACVM_SSH"]
# rsync inserts its dummy host before the remote rsync command.
os.execv(helper, [helper, shlex.join(sys.argv[2:])])
