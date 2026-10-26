#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  up|start|restart|stop|down|kill|rm|create|run|build)
    case " $* " in
      *" gateway "*) echo '正式 Gateway 必須用安全部署入口，新版驗收前不得取代或停止舊版。' >&2; exit 2 ;;
      *" postgres "*) ;;
      *) echo '未明確指定資料庫；拒絕可能中斷 Gateway 的整組操作。' >&2; exit 2 ;;
    esac
    ;;
esac
