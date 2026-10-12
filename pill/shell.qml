// hyprvoice pill — audio-only notch for Hyprland.
//
// Standalone `qs -c hyprvoice-pill` Overlay layer: always above fullscreen,
// zero exclusive zone (never moves windows), tight click box (only the pill).
// Click → `hyprvoice toggle` (start/stop). Status polled from
// $XDG_RUNTIME_DIR/hyprvoice-state.json written by the Rust daemon.
//
// Layer note: Caelestia's ContentWindow sits at WlrLayer.Top (hidden under
// fullscreen); Overlay is the only layer above fullscreen on Hyprland.

import Quickshell
import Quickshell.Io
import Quickshell.Wayland
import QtQuick

ShellRoot {
    id: root

    property string status: "idle"      // idle | recording | transcribing
    property string lastText: ""
    property int recSecs: 0

    readonly property string runtimeDir: Quickshell.env("XDG_RUNTIME_DIR") || "/tmp"
    readonly property string stateFile: runtimeDir + "/hyprvoice-state.json"
    // Installed binary first, repo debug build as fallback.
    readonly property string hvBin: Quickshell.env("HOME") + "/.local/bin/hyprvoice"

    function pollState() {
        stateProc.running = true;
    }

    function toggle() {
        toggleProc.running = true;
        // Optimistic flip so the pill reacts instantly; poll corrects us.
        if (root.status === "recording") {
            root.status = "transcribing";
        } else if (root.status === "idle") {
            root.status = "recording";
            root.recSecs = 0;
        }
    }

    Process {
        id: stateProc
        command: ["sh", "-c", "cat '" + root.stateFile + "' 2>/dev/null || echo '{\"status\":\"idle\"}'"]
        stdout: StdioCollector {
            onStreamFinished: {
                try {
                    var s = JSON.parse(text);
                    if (s.status) root.status = s.status;
                    if (s.text) root.lastText = s.text;
                } catch (e) {}
            }
        }
    }

    Process {
        id: toggleProc
        command: ["sh", "-c", "HYPRVOICE_BIN='" + root.hvBin + "'; [ -x \"$HYPRVOICE_BIN\" ] || HYPRVOICE_BIN='" + Quickshell.env("HOME") + "/.t3/projects/hyprvoice/target/debug/hyprvoice'; \"$HYPRVOICE_BIN\" toggle"]
    }

    Timer {
        interval: 500
        running: true
        repeat: true
        onTriggered: root.pollState()
    }

    Timer {
        interval: 1000
        running: root.status === "recording"
        repeat: true
        onTriggered: root.recSecs += 1
    }

    Variants {
        model: Quickshell.screens

        PanelWindow {
            required property ShellScreen modelData
            screen: modelData

            // The whole point: above fullscreen, never reserves space.
            WlrLayershell.layer: WlrLayer.Overlay
            WlrLayershell.exclusionMode: ExclusionMode.Ignore
            WlrLayershell.namespace: "hyprvoice-pill"
            exclusiveZone: 0

            // Anchor TOP only => top-center notch (layer-shell centers on the edge).
            anchors {
                top: true
            }
            margins {
                top: 8
            }

            implicitWidth: pillBox.implicitWidth
            implicitHeight: 38
            color: "transparent"

            Rectangle {
                id: pillBox
                // Width adapts to state; idle is a compact notch.
                implicitWidth: pillRow.implicitWidth + 28
                implicitHeight: 38
                radius: 19
                color: root.status === "recording" ? "#E01A1A1A" : "#D90B0B0C"
                border.width: 1
                border.color: root.status === "recording" ? "#FF453A" : "#262628"

                Row {
                    id: pillRow
                    anchors.centerIn: parent
                    spacing: 8

                    // Mic / status dot
                    Rectangle {
                        id: dot
                        width: 10
                        height: 10
                        radius: 5
                        anchors.verticalCenter: parent.verticalCenter
                        color: root.status === "recording" ? "#FF453A"
                            : root.status === "transcribing" ? "#FFD60A" : "#30D158"

                        SequentialAnimation on opacity {
                            running: root.status !== "idle"
                            loops: Animation.Infinite
                            NumberAnimation { to: 0.35; duration: 500; easing.type: Easing.InOutQuad }
                            NumberAnimation { to: 1.0; duration: 500; easing.type: Easing.InOutQuad }
                        }
                    }

                    Text {
                        anchors.verticalCenter: parent.verticalCenter
                        color: "#F4F4F5"
                        font.pixelSize: 13
                        font.bold: true
                        text: {
                            if (root.status === "recording") {
                                var m = Math.floor(root.recSecs / 60);
                                var s = root.recSecs % 60;
                                return "● REC " + m + ":" + (s < 10 ? "0" : "") + s;
                            }
                            if (root.status === "transcribing") return "… transcribing";
                            return "🎙 voice";
                        }
                    }
                }

                MouseArea {
                    anchors.fill: parent
                    cursorShape: Qt.PointingHandCursor
                    hoverEnabled: true
                    onClicked: root.toggle()
                }
            }
        }
    }
}
