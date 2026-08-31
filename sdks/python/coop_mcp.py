"""Compatibility imports for the pre-Rookhold ``coop_mcp`` module name."""

import rookhold_mcp as _rookhold_mcp
from rookhold_mcp import *  # noqa: F403

CoopMcpServer = _rookhold_mcp.RookholdMcpServer
__version__ = _rookhold_mcp.__version__
main = _rookhold_mcp.main


if __name__ == "__main__":
    main()
