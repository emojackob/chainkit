"""Embeddable project-scoped indexing. Library callers own authorization.

The HTTP adapter enforces project tokens; Store is a trusted in-process API.
"""
from .errors import IndexerError
from .scanner import Scanner
from .store import Store

__all__ = ["Store", "Scanner", "IndexerError"]
