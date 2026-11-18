#pragma once

#include <QImage>
#include <QJsonObject>
#include <QMutex>
#include <QProcess>
#include <QQuickImageProvider>
#include <QTemporaryDir>
#include <QUrl>
#include <QVariant>
#include <optional>

class PreviewProvider final : public QQuickImageProvider {
public:
    PreviewProvider();
    QImage requestImage(const QString &, QSize *size, const QSize &) override;
    void setImage(QImage image);

private:
    QMutex mutex_;
    QImage image_;
};

class Controller final : public QObject {
    Q_OBJECT
    Q_PROPERTY(bool ready READ ready NOTIFY changed)
    Q_PROPERTY(bool opening READ opening NOTIFY changed)
    Q_PROPERTY(QString inputPath READ inputPath NOTIFY changed)
    Q_PROPERTY(int frameCount READ frameCount NOTIFY changed)
    Q_PROPERTY(int frame READ frame NOTIFY changed)
    Q_PROPERTY(QString frameName READ frameName NOTIFY changed)
    Q_PROPERTY(QUrl image READ image NOTIFY changed)
    Q_PROPERTY(bool rendering READ rendering NOTIFY changed)
    Q_PROPERTY(QString previewError READ previewError NOTIFY changed)
    Q_PROPERTY(QString actionError READ actionError NOTIFY changed)
    Q_PROPERTY(QVariantList colors READ colors NOTIFY changed)
    Q_PROPERTY(int colorIndex READ colorIndex NOTIFY changed)
    Q_PROPERTY(QVariantList exportPresets READ exportPresets NOTIFY exportPresetsChanged)
    Q_PROPERTY(int exportPresetIndex READ exportPresetIndex NOTIFY changed)
    Q_PROPERTY(int whiteBalanceIndex READ whiteBalanceIndex NOTIFY changed)
    Q_PROPERTY(QString lensModel READ lensModel NOTIFY changed)
    Q_PROPERTY(bool lensSupported READ lensSupported NOTIFY changed)
    Q_PROPERTY(bool lensEnabled READ lensEnabled NOTIFY changed)
    Q_PROPERTY(QString fps READ fps NOTIFY changed)
    Q_PROPERTY(bool fpsValid READ fpsValid NOTIFY changed)
    Q_PROPERTY(bool canExport READ canExport NOTIFY changed)
    Q_PROPERTY(bool exporting READ exporting NOTIFY changed)
    Q_PROPERTY(int exportPercent READ exportPercent NOTIFY progressChanged)
    Q_PROPERTY(QString exportPhase READ exportPhase NOTIFY progressChanged)
    Q_PROPERTY(int outstanding READ outstanding NOTIFY changed)
    Q_PROPERTY(bool canClear READ canClear NOTIFY changed)
    Q_PROPERTY(QVariantList jobs READ jobs NOTIFY changed)

public:
    explicit Controller(PreviewProvider &provider);
    ~Controller() override;

    bool ready() const { return !baseProfile_.isEmpty(); }
    bool opening() const { return opening_; }
    QString inputPath() const { return inputPath_; }
    int frameCount() const { return frameCount_; }
    int frame() const { return frame_; }
    QString frameName() const { return frameName_; }
    QUrl image() const { return image_; }
    bool rendering() const { return rendering_; }
    QString previewError() const { return previewError_; }
    QString actionError() const { return actionError_; }
    QVariantList colors() const;
    int colorIndex() const { return colorIndex_; }
    QVariantList exportPresets() const { return exportPresets_; }
    int exportPresetIndex() const { return exportPresetIndex_; }
    int whiteBalanceIndex() const { return whiteBalanceIndex_; }
    QString lensModel() const { return lensModel_; }
    bool lensSupported() const { return lensSupported_; }
    bool lensEnabled() const { return lensEnabled_ && !lensModel_.isEmpty(); }
    QString fps() const { return fps_; }
    bool fpsValid() const;
    bool canExport() const;
    bool exporting() const { return outstanding() > 0; }
    int exportPercent() const { return exportPercent_; }
    QString exportPhase() const { return exportPhase_; }
    int outstanding() const;
    bool canClear() const;
    QVariantList jobs() const;

    Q_INVOKABLE void openFolder(const QUrl &url);
    Q_INVOKABLE void closeSource();
    Q_INVOKABLE void selectFrame(int frame);
    Q_INVOKABLE void selectWhiteBalance(int index);
    Q_INVOKABLE void selectColor(int index);
    Q_INVOKABLE void selectExportPreset(int index);
    Q_INVOKABLE void setLensEnabled(bool enabled);
    Q_INVOKABLE void setFps(const QString &fps);
    Q_INVOKABLE void enqueueExport();
    Q_INVOKABLE void clearFinished();
    Q_INVOKABLE void openOutputFolder(int id);
    Q_INVOKABLE void dismissError();

signals:
    void changed();
    void exportPresetsChanged();
    void progressChanged();

private:
    enum class Request { None, Hello, Open, Close, Preview };
    enum class JobState { Waiting, Running, Done, Failed };
    struct Job {
        int id;
        QString input;
        QString output;
        QJsonObject profile;
        JobState state = JobState::Waiting;
        QString error;
    };

    PreviewProvider &provider_;
    QTemporaryDir scratch_;
    QProcess worker_;
    QProcess hardwareProbe_;
    QProcess encoder_;
    QString cliPath_, nativePath_, exiftool_, converter_, ffmpeg_, ffprobe_;
    QJsonObject baseProfile_;
    QStringList colorIds_;
    QStringList exportPresetIds_;
    QVariantList exportPresets_;
    QByteArray previewReplies_, workerError_, encoderError_;
    Request activeRequest_ = Request::None;
    std::optional<QString> queuedSource_;
    bool closePending_ = false;
    bool shuttingDown_ = false;
    bool opening_ = false;
    bool rendering_ = false;
    bool dirty_ = false;
    quint64 previewVersion_ = 0;
    quint64 dispatchedVersion_ = 0;
    QString inputPath_, outputPath_, autoLog_, autoError_, frameName_, lensModel_;
    QUrl image_;
    int frameCount_ = 0;
    int frame_ = 1;
    int colorIndex_ = 0;
    int exportPresetIndex_ = -1;
    int whiteBalanceIndex_ = 0;
    bool lensSupported_ = false;
    bool lensEnabled_ = true;
    QString fps_ = QStringLiteral("60");
    QString previewError_, actionError_;
    QList<Job> jobs_;
    int nextJobId_ = 0;
    int activeJob_ = 0;
    int exportPercent_ = 0;
    QString exportPhase_;
    QByteArray exportReplies_;

    void startWorker();
    void onWorkerReply(const QJsonObject &reply);
    void workerFailed(const QString &error);
    void send(Request request, QJsonObject message);
    void pumpPreview();
    void requestPreview();
    void resetSource();
    QString selectedColor() const;
    std::optional<QJsonValue> profileFps() const;
    QJsonObject exportProfile(const QJsonValue &fps) const;
    QString availableOutputPath() const;
    void readExportProgress();
    void runNextExport();
    void finishExport(bool success, const QString &error);
    void setError(const QString &error);
};
