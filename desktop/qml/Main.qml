pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Controls
import QtQuick.Dialogs
import QtQuick.Layouts

ApplicationWindow {
    id: window
    required property var workspace

    width: 1120
    height: 720
    minimumWidth: 720
    minimumHeight: 500
    visible: true
    title: "RAW Timelapse"
    color: "#191c1f"

    readonly property color surface: "#21262a"
    readonly property color borderColor: "#343d42"
    readonly property color muted: "#aeb8bb"
    readonly property color accent: "#9acdb7"
    readonly property color foreground: "#e8eeec"
    readonly property bool hasSource: workspace.frameCount > 0
    property bool exitConfirmed: false

    onClosing: function(event) {
        if (workspace.exporting && !exitConfirmed) {
            event.accepted = false
            queuePopup.close()
            exitDialog.open()
        }
    }

    palette.window: window.color
    palette.windowText: window.foreground
    palette.base: "#272e33"
    palette.alternateBase: window.surface
    palette.text: window.foreground
    palette.button: "#2b3338"
    palette.buttonText: window.foreground
    palette.highlight: window.accent
    palette.highlightedText: "#182923"

    component SectionHeading: Label {
        font.weight: Font.DemiBold
    }

    component PathLabel: Label {
        elide: Text.ElideMiddle
        maximumLineCount: 1
        textFormat: Text.PlainText
        ToolTip.visible: truncated && hover.hovered
        ToolTip.delay: 350
        ToolTip.text: text
        HoverHandler { id: hover }
    }

    FolderDialog {
        id: directoryDialog
        title: qsTr("选择 RAW 素材文件夹")
        onAccepted: window.workspace.openFolder(selectedFolder)
    }

    Dialog {
        id: exitDialog
        objectName: "exitWhileExporting"
        anchors.centerIn: parent
        width: Math.min(420, window.width - 32)
        modal: true
        title: qsTr("退出 RAW Timelapse？")
        contentItem: Label {
            text: qsTr("退出会中止正在导出的任务，并丢弃队列中等待的任务。")
            wrapMode: Text.WordWrap
        }
        footer: DialogButtonBox {
            Button {
                objectName: "keepExporting"
                text: qsTr("继续导出")
                DialogButtonBox.buttonRole: DialogButtonBox.RejectRole
            }
            Button {
                objectName: "confirmExportExit"
                text: qsTr("退出")
                DialogButtonBox.buttonRole: DialogButtonBox.AcceptRole
            }
        }
        onAccepted: {
            window.exitConfirmed = true
            window.close()
        }
    }

    header: Rectangle {
        implicitHeight: 54
        color: window.surface

        Rectangle {
            anchors.bottom: parent.bottom
            width: parent.width
            height: 1
            color: window.borderColor
        }
        RowLayout {
            anchors.fill: parent
            anchors.leftMargin: 16
            anchors.rightMargin: 16
            spacing: 10

            Label {
                text: "RAW Timelapse"
                font.weight: Font.DemiBold
                Layout.rightMargin: 5
            }
            Button {
                text: qsTr("打开文件夹")
                enabled: window.workspace.ready && !window.workspace.opening
                onClicked: directoryDialog.open()
            }
            PathLabel {
                Layout.fillWidth: true
                color: window.muted
                text: window.workspace.inputPath
            }
            Button {
                text: qsTr("关闭素材")
                visible: window.hasSource
                enabled: !window.workspace.opening
                onClicked: window.workspace.closeSource()
            }
            Button {
                id: queueButton
                objectName: "queueButton"
                text: window.workspace.outstanding > 0
                    ? qsTr("导出队列 · %1").arg(window.workspace.outstanding)
                    : qsTr("导出队列")
                onClicked: queuePopup.visible ? queuePopup.close() : queuePopup.open()
            }
        }
    }

    Popup {
        id: queuePopup
        objectName: "exportQueue"
        parent: queueButton
        x: queueButton.width - width
        y: queueButton.height + 12
        width: Math.min(440, window.width - 32)
        focus: true
        closePolicy: Popup.CloseOnEscape | Popup.CloseOnPressOutsideParent
        padding: 0

        background: Rectangle {
            color: window.surface
            border.color: window.borderColor
            radius: 8
        }

        contentItem: ColumnLayout {
            spacing: 0
            RowLayout {
                Layout.fillWidth: true
                Layout.margins: 12
                SectionHeading {
                    Layout.fillWidth: true
                    text: qsTr("导出队列")
                }
                Button {
                    objectName: "clearQueue"
                    text: qsTr("清除所有")
                    enabled: window.workspace.canClear
                    onClicked: window.workspace.clearFinished()
                    ToolTip.visible: hovered
                    ToolTip.delay: 350
                    ToolTip.text: qsTr("清除已结束的导出记录")
                }
            }
            Label {
                Layout.fillWidth: true
                Layout.margins: 12
                visible: tasks.count === 0
                text: qsTr("暂无导出任务")
                color: window.muted
            }
            ListView {
                id: tasks
                Layout.fillWidth: true
                Layout.preferredHeight: Math.min(contentHeight, 278)
                visible: count > 0
                clip: true
                model: window.workspace.jobs
                ScrollBar.vertical: ScrollBar {}
                delegate: Item {
                    required property var modelData
                    width: tasks.width
                    implicitHeight: details.implicitHeight + 18

                    Rectangle {
                        width: parent.width
                        height: 1
                        color: window.borderColor
                    }
                    RowLayout {
                        id: details
                        anchors.fill: parent
                        anchors.leftMargin: 12
                        anchors.rightMargin: 12
                        anchors.topMargin: 9
                        anchors.bottomMargin: 9
                        spacing: 10

                        ColumnLayout {
                            Layout.fillWidth: true
                            spacing: 2
                            PathLabel {
                                Layout.fillWidth: true
                                text: modelData.path
                                font.weight: Font.DemiBold
                            }
                            PathLabel {
                                Layout.fillWidth: true
                                text: modelData.running
                                    ? qsTr("%1 · %2%").arg(window.workspace.exportPhase).arg(window.workspace.exportPercent)
                                    : modelData.error ? modelData.status + "  " + modelData.error : modelData.status
                                color: modelData.running ? window.accent : window.muted
                            }
                            ProgressBar {
                                objectName: "exportProgress"
                                Layout.fillWidth: true
                                visible: modelData.running
                                from: 0
                                to: 100
                                value: window.workspace.exportPercent
                            }
                        }
                        Button {
                            objectName: "revealOutputFolder"
                            visible: modelData.finished
                            Layout.preferredWidth: 36
                            icon.source: "folder.svg"
                            icon.color: "transparent"
                            icon.width: 18
                            icon.height: 18
                            Accessible.name: qsTr("打开所在文件夹")
                            ToolTip.visible: hovered
                            ToolTip.text: Accessible.name
                            onClicked: window.workspace.openOutputFolder(modelData.jobId)
                        }
                    }
                }
            }
        }
    }

    RowLayout {
        anchors.fill: parent
        spacing: 0

        ColumnLayout {
            Layout.fillWidth: true
            Layout.fillHeight: true
            spacing: 0

            RowLayout {
                Layout.fillWidth: true
                Layout.leftMargin: 16
                Layout.rightMargin: 16
                Layout.preferredHeight: 36
                SectionHeading { text: qsTr("预览") }
                PathLabel {
                    Layout.fillWidth: true
                    horizontalAlignment: Text.AlignRight
                    text: window.workspace.frameName
                    color: window.muted
                }
            }
            Rectangle {
                Layout.fillWidth: true
                Layout.fillHeight: true
                color: "#0f1214"

                Image {
                    id: picture
                    objectName: "previewPicture"
                    anchors.fill: parent
                    anchors.margins: 8
                    source: window.workspace.image
                    fillMode: Image.PreserveAspectFit
                    cache: false
                }
                Button {
                    anchors.centerIn: parent
                    visible: !window.hasSource
                    enabled: window.workspace.ready && !window.workspace.opening
                    text: window.workspace.opening ? qsTr("正在打开素材") : qsTr("选择文件夹")
                    onClicked: directoryDialog.open()
                }
                Label {
                    anchors.right: parent.right
                    anchors.top: parent.top
                    anchors.margins: 12
                    width: Math.min(implicitWidth, parent.width - 24)
                    visible: window.workspace.rendering || window.workspace.previewError.length > 0
                    text: window.workspace.rendering ? qsTr("渲染中") : window.workspace.previewError
                    textFormat: Text.PlainText
                    wrapMode: Text.Wrap
                    padding: 8
                    background: Rectangle { color: "#ee242a2e"; radius: 5 }
                }
            }
            Rectangle {
                visible: window.hasSource
                Layout.fillWidth: true
                height: 1
                color: window.borderColor
            }
            ColumnLayout {
                visible: window.hasSource
                Layout.fillWidth: true
                Layout.margins: 14
                spacing: 3

                RowLayout {
                    Layout.fillWidth: true
                    SectionHeading { text: qsTr("时间线") }
                    Label {
                        Layout.fillWidth: true
                        horizontalAlignment: Text.AlignRight
                        text: qsTr("%1 / %2 帧").arg(window.workspace.frame).arg(window.workspace.frameCount)
                        color: window.muted
                    }
                }
                RowLayout {
                    Layout.fillWidth: true
                    Layout.leftMargin: 7
                    Layout.rightMargin: 7
                    Label { text: "1"; color: window.muted }
                    Item { Layout.fillWidth: true }
                    Label {
                        visible: window.workspace.frameCount > 1
                        text: window.workspace.frameCount
                        color: window.muted
                    }
                }
                Slider {
                    id: timeline
                    objectName: "timeline"
                    Layout.fillWidth: true
                    Layout.preferredHeight: 30
                    enabled: window.workspace.frameCount > 1
                    leftPadding: 5
                    rightPadding: 5
                    from: 1
                    to: Math.max(1, window.workspace.frameCount)
                    value: window.workspace.frame
                    stepSize: 1
                    snapMode: Slider.SnapAlways
                    live: true
                    Accessible.name: qsTr("当前帧")
                    onMoved: window.workspace.selectFrame(Math.round(value))
                    background: Rectangle {
                        x: timeline.leftPadding + 2
                        y: (timeline.height - height) / 2
                        width: Math.max(0, timeline.availableWidth - 4)
                        height: 4
                        radius: 2
                        color: "#495457"
                    }
                    handle: Rectangle {
                        x: timeline.leftPadding + timeline.visualPosition * (timeline.availableWidth - width)
                        y: (timeline.height - height) / 2
                        width: 4
                        height: 18
                        radius: 2
                        color: window.accent
                    }
                }
            }
        }

        Rectangle {
            Layout.fillHeight: true
            width: 1
            color: window.borderColor
        }
        Rectangle {
            Layout.fillHeight: true
            Layout.preferredWidth: Math.min(350, Math.max(260, window.width * 0.29))
            color: window.surface
            ColumnLayout {
                anchors.fill: parent
                anchors.margins: 16
                spacing: 8
                SectionHeading {
                    text: qsTr("预处理")
                    Layout.bottomMargin: 4
                }
                Label { text: qsTr("白平衡") }
                ComboBox {
                    objectName: "whiteBalance"
                    Layout.fillWidth: true
                    enabled: window.hasSource
                    model: [qsTr("使用首帧"), qsTr("使用各帧拍摄值")]
                    currentIndex: window.workspace.whiteBalanceIndex
                    Accessible.name: qsTr("白平衡")
                    onActivated: index => window.workspace.selectWhiteBalance(index)
                }
                CheckBox {
                    Layout.topMargin: 4
                    text: qsTr("镜头校正")
                    enabled: window.hasSource && window.workspace.lensModel.length > 0
                    checked: window.workspace.lensEnabled
                    onToggled: window.workspace.setLensEnabled(checked)
                }
                Label {
                    Layout.fillWidth: true
                    Layout.leftMargin: 23
                    color: window.muted
                    wrapMode: Text.Wrap
                    text: window.workspace.lensModel.length === 0 ? qsTr("无镜头信息")
                        : window.workspace.lensModel + (window.workspace.lensSupported ? "" : qsTr("（不支持校正）"))
                }
                Rectangle {
                    Layout.fillWidth: true
                    Layout.topMargin: 8
                    Layout.bottomMargin: 8
                    height: 1
                    color: window.borderColor
                }
                SectionHeading {
                    text: qsTr("导出")
                    Layout.bottomMargin: 4
                }
                Label { text: qsTr("色彩") }
                ComboBox {
                    objectName: "outputColor"
                    Layout.fillWidth: true
                    enabled: window.hasSource
                    model: window.workspace.colors
                    currentIndex: window.workspace.colorIndex
                    Accessible.name: qsTr("色彩")
                    onActivated: index => window.workspace.selectColor(index)
                }
                Label {
                    Layout.topMargin: 4
                    text: qsTr("输出帧率")
                }
                RowLayout {
                    TextField {
                        objectName: "fps"
                        Layout.preferredWidth: 95
                        enabled: window.hasSource
                        text: window.workspace.fps
                        inputMethodHints: Qt.ImhFormattedNumbersOnly
                        Accessible.name: qsTr("输出帧率")
                        onTextEdited: window.workspace.setFps(text)
                    }
                    Label { text: "fps"; color: window.muted }
                }
                Label {
                    visible: !window.workspace.fpsValid
                    Layout.fillWidth: true
                    text: qsTr("请输入大于 0 且不超过 1000 的数字。")
                    wrapMode: Text.Wrap
                    color: "#ffaaa2"
                }
                Item { Layout.fillHeight: true }
            }
        }
    }

    footer: Rectangle {
        implicitHeight: 55
        color: window.surface
        Rectangle {
            width: parent.width
            height: 1
            color: window.borderColor
        }
        RowLayout {
            anchors.fill: parent
            anchors.leftMargin: 16
            anchors.rightMargin: 16
            spacing: 10
            PathLabel {
                Layout.fillWidth: true
                text: window.workspace.actionError
                color: "#ffaaa2"
            }
            Button {
                visible: window.workspace.actionError.length > 0
                text: qsTr("关闭提示")
                onClicked: window.workspace.dismissError()
            }
            Button {
                objectName: "exportButton"
                highlighted: true
                text: window.workspace.exporting ? qsTr("加入导出队列") : qsTr("导出")
                enabled: window.workspace.canExport
                onClicked: window.workspace.enqueueExport()
            }
        }
    }
}
