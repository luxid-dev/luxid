#!/usr/bin/env bash

for c in luxid-macros luxid-core luxid-orm luxid-testing luxid-cli luxid; do
  cargo publish -p $c || break
  sleep 45
done
