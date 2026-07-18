# Reference native escape

This package deliberately declares a native built-in adapter. Installable
packages are never allowed to do that: preview must reject it before the host
registry or runtime is changed.

It is test material, not an example extension to install for normal use.
