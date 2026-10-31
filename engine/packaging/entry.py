"""PyInstaller giriş noktası (paketli `subtitle-engine` ikilisi)."""
import multiprocessing
import sys

from subtitle_engine.cli import main

if __name__ == "__main__":
    multiprocessing.freeze_support()
    sys.exit(main())
