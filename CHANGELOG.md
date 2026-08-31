# Changelog

## [0.2.0](https://github.com/chrizzlekicks/bergr/compare/bergr-v0.1.0...bergr-v0.2.0) (2026-08-31)


### Features

* **bergr:** port amux prototype to a Rust CLI ([ac3ff45](https://github.com/chrizzlekicks/bergr/commit/ac3ff45b166e447073dfccc48611b082ebf7ff03))


### Bug Fixes

* **bergr:** harden event() against races, injection, and silent timeouts ([aa97a35](https://github.com/chrizzlekicks/bergr/commit/aa97a350ac212c1c43e55b7ae4a9cc83656d0677))
* **bergr:** match windows by stable id and server pid, unify state pruning ([553e3db](https://github.com/chrizzlekicks/bergr/commit/553e3dbd278ab87e7590fcdcf2c9ae2f825b632d))
* **bergr:** preserve permissions and encode state path components ([957e149](https://github.com/chrizzlekicks/bergr/commit/957e14992c269d441ca09d9f8661f1a96f5f793c))
* **bergr:** respect XDG dirs for legacy amux migration ([f4188ae](https://github.com/chrizzlekicks/bergr/commit/f4188ae1246e8f00b9e5796d874a437b1fa73578))
* **bergr:** survive a fresh HOME and address code-review findings ([7fc5c55](https://github.com/chrizzlekicks/bergr/commit/7fc5c552f18ca5e2d0193e7824e8c25055879de7))
* **bergr:** verify amux ownership, quote shell paths, require absolute XDG/HOME ([8b034b4](https://github.com/chrizzlekicks/bergr/commit/8b034b4651108437230477e0caaa04cd60095e70))
