"""Compatibility imports for the pre-Rookhold ``coop`` module name.

New code should import :mod:`rookhold`.  The aliases remain available through
the v0.6 migration so existing applications can upgrade the distribution
without changing their imports atomically.
"""

import rookhold as _rookhold
from rookhold import *  # noqa: F403

__version__ = _rookhold.__version__

Coop = _rookhold.Rookhold
CoopError = _rookhold.RookholdError
CoopEvent = _rookhold.RookholdEvent
HashedCoopEvent = _rookhold.HashedRookholdEvent
