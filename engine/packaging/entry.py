"""PyInstaller entry point (the packaged `subtitle-engine` binary)."""
import multiprocessing
import sys

from subtitle_engine.cli import main

if __name__ == "__main__":
    multiprocessing.freeze_support()
    sys.exit(main())
