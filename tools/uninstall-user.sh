#!/usr/bin/env bash
set -euo pipefail

domain="gui/$(id -u)"
agent="$HOME/Library/LaunchAgents/com.smfmarin.personal-audio-lake.plist"
clickhouse_agent="$HOME/Library/LaunchAgents/com.smfmarin.personal-audio-lake.clickhouse.plist"
launchctl bootout "$domain/com.smfmarin.personal-audio-lake" 2>/dev/null || true
launchctl bootout "$domain/com.smfmarin.personal-audio-lake.clickhouse" 2>/dev/null || true
rm -f "$agent"
rm -f "$clickhouse_agent"
rm -rf "$HOME/Applications/Captain's Log.app"
rm -rf "$HOME/Applications/Personal Audio Lake.app"
echo "Application removed. Private lake data and source recovery credentials were preserved."
echo "Release any application-bound source before moving it to another phone or computer."
