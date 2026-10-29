import QtQuick
import QtQuick.Layouts
import org.kde.plasma.plasmoid
import org.kde.plasma.components as PlasmaComponents
import org.kde.plasma.plasma5support as P5Support
import org.kde.kirigami as Kirigami

PlasmoidItem {
    id: root
    // { panel: "85%", rows: [[label, value], …] } written by Rig Hub; null while it isn't running
    property var status: null
    // pgrep first: the file outlives Rig Hub, and stale numbers would look live
    readonly property string poll: `pgrep -x rig-hub >/dev/null && cat "\${XDG_RUNTIME_DIR:-\${XDG_CACHE_HOME:-$HOME/.cache}}/rig-hub-status.json"`
    // a second launch just brings the running one to the front
    readonly property string open: `PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH" setsid -f rig-hub`

    toolTipMainText: "Rig Hub"
    toolTipSubText: status ? status.rows.map(r => `${r[0]}  ${r[1]}`).join("\n") : "Rig Hub isn't running"

    P5Support.DataSource {
        id: exec
        engine: "executable"
        onNewData: (source, data) => {
            disconnectSource(source)
            if (source !== root.poll)
                return
            try {
                root.status = data.stdout ? JSON.parse(data.stdout) : null
            } catch (e) {
                root.status = null
            }
        }
    }

    Timer {
        interval: 5000
        running: true
        repeat: true
        triggeredOnStart: true
        onTriggered: exec.connectSource(root.poll)
    }

    compactRepresentation: MouseArea {
        Layout.minimumWidth: compact.implicitWidth
        onClicked: root.expanded = !root.expanded
        RowLayout {
            id: compact
            anchors.centerIn: parent
            spacing: Kirigami.Units.smallSpacing
            Kirigami.Icon {
                source: "audio-headset"
                Layout.preferredWidth: Kirigami.Units.iconSizes.small
                Layout.preferredHeight: Kirigami.Units.iconSizes.small
                opacity: root.status ? 1 : 0.5
            }
            PlasmaComponents.Label {
                visible: text !== ""
                text: root.status ? root.status.panel : ""
                font.bold: true
            }
        }
    }

    fullRepresentation: ColumnLayout {
        Layout.preferredWidth: Kirigami.Units.gridUnit * 16
        spacing: Kirigami.Units.smallSpacing

        Kirigami.Heading {
            level: 4
            text: "Rig Hub"
        }
        Repeater {
            model: root.status ? root.status.rows : []
            delegate: RowLayout {
                required property var modelData
                Layout.fillWidth: true
                PlasmaComponents.Label {
                    text: modelData[0]
                    Layout.fillWidth: true
                }
                PlasmaComponents.Label {
                    text: modelData[1]
                    font.bold: true
                }
            }
        }
        PlasmaComponents.Label {
            visible: !root.status
            text: "Rig Hub isn't running."
            opacity: 0.6
        }
        PlasmaComponents.Button {
            Layout.topMargin: Kirigami.Units.smallSpacing
            Layout.fillWidth: true
            icon.name: "window-new"
            text: root.status ? "Open Rig Hub" : "Start Rig Hub"
            onClicked: {
                exec.connectSource(root.open)
                root.expanded = false
            }
        }
        Item { Layout.fillHeight: true }
    }
}
