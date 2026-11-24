#!/bin/sh
# Runs once, on an empty data volume: one database and role per application.
set -eu

psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" \
  -v endurain_pw="$ENDURAIN_DB_PASSWORD" \
  -v running_viz_pw="$RUNNING_VIZ_DB_PASSWORD" <<'EOSQL'
CREATE ROLE endurain LOGIN PASSWORD :'endurain_pw';
CREATE DATABASE endurain OWNER endurain;
CREATE ROLE running_viz LOGIN PASSWORD :'running_viz_pw';
CREATE DATABASE running_viz OWNER running_viz;
EOSQL
