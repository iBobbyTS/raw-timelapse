#include "controller.h"

#include <QCoreApplication>
#include <QDesktopServices>
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QHash>
#include <QMutexLocker>
#include <QProcessEnvironment>
#include <QJsonArray>
#include <QJsonDocument>
#include <QRegularExpression>
#include <QSaveFile>
#include <QStandardPaths>
#include <QTimer>
#include <algorithm>
#include <numeric>
#include <utility>

namespace {
QString executable(const char *environment, const QString &name, const QString &fallback = {}) {
    const QString override = qEnvironmentVariable(environment);
    if (!override.isEmpty())
        return QFileInfo(override).absoluteFilePath();
#ifdef Q_OS_WIN
    const QString filename = name + ".exe";
#else
    const QString filename = name;
#endif
    const QString sibling = QDir(QCoreApplication::applicationDirPath()).filePath(filename);
    if (QFileInfo(sibling).isExecutable())
        return sibling;
    if (QFileInfo(fallback).isExecutable())
        return QDir::cleanPath(fallback);
    const QString installed = QStandardPaths::findExecutable(filename);
    return installed.isEmpty() ? (fallback.isEmpty() ? sibling : fallback) : installed;
}

QString colorName(const QString &id, bool shortName = false) {
    static const QHash<QString, QPair<QString, QString>> names = {
        {"srgb-preview", {"sRGB", {}}},
        {"clog2", {"Canon Log 2", "Cinema Gamut"}},
        {"clog3", {"Canon Log 3", "Cinema Gamut"}},
        {"dji-dlog-dgamut", {"DJI D-Log", "D-Gamut"}},
        {"dji-dlog2-dgamut2", {"DJI D-Log 2", "D-Gamut 2"}},
        {"nikon-nlog-bt2020", {"Nikon N-Log", "BT.2020"}},
        {"sony-slog3-sgamut3", {"Sony S-Log3", "S-Gamut3"}},
        {"sony-slog3-sgamut3-cine", {"Sony S-Log3", "S-Gamut3.Cine"}},
    };
    const auto name = names.value(id, {id, {}});
    return shortName || name.second.isEmpty() ? name.first : name.first + " / " + name.second;
}

void appendTail(QByteArray &tail, const QByteArray &chunk) {
    tail = (tail + chunk).right(8192);
}

QString desktopPath(QString path) {
#ifdef Q_OS_WIN
    path = QDir::fromNativeSeparators(path);
    // Rust canonicalization may use verbatim DOS or UNC paths. File URLs
    // and the path label use the usual drive or UNC spelling.
    if (path.startsWith("//?/UNC/", Qt::CaseInsensitive))
        return "//" + path.sliced(8);
    if (path.startsWith("//?/"))
        return path.sliced(4);
#endif
    return path;
}

void terminateProcess(QProcess &process) {
    if (process.state() == QProcess::NotRunning)
        return;
    process.terminate();
    if (!process.waitForFinished(5000)) {
        process.kill();
        process.waitForFinished(1000);
    }
}
}

PreviewProvider::PreviewProvider() : QQuickImageProvider(QQuickImageProvider::Image) {}

QImage PreviewProvider::requestImage(const QString &, QSize *size, const QSize &) {
    QMutexLocker lock(&mutex_);
    if (size)
        *size = image_.size();
    return image_;
}

void PreviewProvider::setImage(QImage image) {
    QMutexLocker lock(&mutex_);
    image_ = std::move(image);
}

Controller::Controller(PreviewProvider &provider) : provider_(provider) {
    cliPath_ = executable("RAW_TIMELAPSE_CLI", "rawtl-linear-video", RAWTL_CLI_FALLBACK);
    nativePath_ = executable("RAW_TIMELAPSE_NATIVE", "dng-linear-frame", RAWTL_NATIVE_FALLBACK);
    exiftool_ = executable("RAW_TIMELAPSE_EXIFTOOL", "exiftool"
#ifdef Q_OS_MACOS
                          , "/opt/homebrew/bin/exiftool"
#endif
    );
    ffmpeg_ = executable("RAW_TIMELAPSE_FFMPEG", "ffmpeg"
#ifdef Q_OS_MACOS
                        , "/opt/homebrew/opt/ffmpeg-full/bin/ffmpeg"
#endif
    );
    ffprobe_ = executable("RAW_TIMELAPSE_FFPROBE", "ffprobe"
#ifdef Q_OS_MACOS
                         , "/opt/homebrew/opt/ffmpeg-full/bin/ffprobe"
#endif
    );
    converter_ = qEnvironmentVariable("RAW_TIMELAPSE_CONVERTER");

    const QString workerPath = executable("RAW_TIMELAPSE_WORKER", "rawtl-desktop-worker", RAWTL_WORKER_FALLBACK);
    worker_.setProgram(workerPath);
    worker_.setArguments({scratch_.path(), nativePath_, exiftool_, converter_});
    connect(&worker_, &QProcess::started, this, [this] { send(Request::Hello, {{"command", "hello"}}); });
    connect(&worker_, &QProcess::readyReadStandardError, this, [this] {
        appendTail(workerError_, worker_.readAllStandardError());
    });
    connect(&worker_, &QProcess::readyReadStandardOutput, this, [this] {
        previewReplies_.append(worker_.readAllStandardOutput());
        for (qsizetype end = previewReplies_.indexOf('\n'); end >= 0; end = previewReplies_.indexOf('\n')) {
            const auto reply = QJsonDocument::fromJson(previewReplies_.left(end));
            previewReplies_.remove(0, end + 1);
            if (!reply.isObject()) {
                workerFailed(tr("预览进程返回了无效响应。"));
                return;
            }
            onWorkerReply(reply.object());
        }
    });
    connect(&worker_, &QProcess::errorOccurred, this, [this](QProcess::ProcessError error) {
        if (error == QProcess::FailedToStart)
            workerFailed(tr("无法启动预览进程：%1").arg(worker_.errorString()));
    });
    connect(&worker_, &QProcess::finished, this, [this](int, QProcess::ExitStatus) {
        if (!shuttingDown_)
            workerFailed(tr("预览进程已退出。%1").arg(QString::fromUtf8(workerError_).trimmed()));
    });

    auto exportEnvironment = QProcessEnvironment::systemEnvironment();
    exportEnvironment.insert("RAWTL_DESKTOP_PROGRESS", "1");
    encoder_.setProcessEnvironment(exportEnvironment);
    connect(&encoder_, &QProcess::readyReadStandardOutput, this, &Controller::readExportProgress);
    connect(&encoder_, &QProcess::readyReadStandardError, this, [this] {
        appendTail(encoderError_, encoder_.readAllStandardError());
    });
    connect(&encoder_, &QProcess::finished, this, [this](int code, QProcess::ExitStatus status) {
        appendTail(encoderError_, encoder_.readAllStandardError());
        const QString detail = QString::fromUtf8(encoderError_).trimmed().right(1600);
        finishExport(status == QProcess::NormalExit && code == 0,
                     detail.isEmpty() ? tr("导出进程退出，代码 %1。").arg(code) : detail);
    });
    connect(&encoder_, &QProcess::errorOccurred, this, [this](QProcess::ProcessError error) {
        if (error == QProcess::FailedToStart)
            finishExport(false, tr("无法启动导出进程：%1").arg(encoder_.errorString()));
    });
    QTimer::singleShot(0, this, &Controller::startWorker);
}

Controller::~Controller() {
    shuttingDown_ = true;
    worker_.disconnect(this);
    encoder_.disconnect(this);
    worker_.closeWriteChannel();
    if (worker_.state() != QProcess::NotRunning && !worker_.waitForFinished(250))
        terminateProcess(worker_);
    terminateProcess(encoder_);
}

void Controller::startWorker() {
    if (!scratch_.isValid()) {
        workerFailed(tr("无法创建临时工作目录。"));
        return;
    }
    previewReplies_.clear();
    workerError_.clear();
    worker_.start();
}

void Controller::send(Request request, QJsonObject message) {
    activeRequest_ = request;
    worker_.write(QJsonDocument(message).toJson(QJsonDocument::Compact) + '\n');
}

void Controller::pumpPreview() {
    if (activeRequest_ != Request::None || worker_.state() != QProcess::Running)
        return;
    if (std::exchange(closePending_, false)) {
        send(Request::Close, {{"command", "close"}});
        return;
    }
    if (queuedSource_) {
        const QString path = std::exchange(queuedSource_, std::nullopt).value();
        send(Request::Open, {{"command", "open"}, {"path", path}});
        return;
    }
    if (!dirty_ || frameCount_ == 0 || opening_)
        return;
    dirty_ = false;
    dispatchedVersion_ = previewVersion_;
    send(Request::Preview, {
        {"command", "preview"}, {"index", frame_ - 1}, {"color", selectedColor()},
        {"white_balance", whiteBalanceIndex_ == 0 ? "first-frame" : "per-frame"},
        {"lens", lensEnabled_},
    });
}

void Controller::onWorkerReply(const QJsonObject &reply) {
    const Request request = std::exchange(activeRequest_, Request::None);
    const auto result = reply.value("result").toObject();
    const QString error = reply.value("error").toString();
    switch (request) {
    case Request::Hello:
        if (!error.isEmpty()) {
            workerFailed(error);
            return;
        }
        if (baseProfile_.isEmpty()) {
            baseProfile_ = result.value("profile").toObject();
            for (const auto &entry : result.value("colors").toArray())
                colorIds_.append(entry.toString());
            if (colorIds_.contains("srgb-preview"))
                colorIds_.insert(colorIds_.indexOf("srgb-preview") + 1, "auto-log");
            colorIndex_ = std::max<qsizetype>(0, colorIds_.indexOf(baseProfile_.value("output_color").toString()));
            fps_ = QString::number(baseProfile_.value("fps").toInt(60));
        }
        break;
    case Request::Open:
        opening_ = false;
        if (!error.isEmpty()) {
            actionError_ = error;
        } else {
            resetSource();
            inputPath_ = QDir::toNativeSeparators(desktopPath(result.value("path").toString()));
            outputPath_ = result.value("output").toString();
            autoLog_ = result.value("auto_log").toString();
            autoError_ = result.value("auto_error").toString();
            frameCount_ = result.value("frames").toInt();
            requestPreview();
        }
        break;
    case Request::Preview:
        if (dispatchedVersion_ != previewVersion_)
            break;
        rendering_ = false;
        if (!error.isEmpty()) {
            previewError_ = error;
        } else {
            QImage frame(result.value("image").toString(), "PPM");
            if (frame.isNull()) {
                previewError_ = tr("无法读取显影画面。");
                break;
            }
            provider_.setImage(std::move(frame));
            image_ = QUrl(QString("image://preview/%1").arg(previewVersion_));
            frameName_ = result.value("name").toString();
            const auto lens = result.value("lens").toObject();
            lensModel_ = lens.value("model").toString();
            lensSupported_ = lens.value("supported").toBool();
        }
        break;
    case Request::Close:
        if (!error.isEmpty())
            actionError_ = error;
        break;
    case Request::None:
        break;
    }
    emit changed();
    pumpPreview();
}

void Controller::workerFailed(const QString &error) {
    if (shuttingDown_)
        return;
    activeRequest_ = Request::None;
    queuedSource_.reset();
    closePending_ = false;
    opening_ = false;
    resetSource();
    setError(error);
}

void Controller::openFolder(const QUrl &url) {
    if (opening_ || !url.isLocalFile())
        return;
    opening_ = true;
    actionError_.clear();
    queuedSource_ = url.toLocalFile();
    if (worker_.state() == QProcess::NotRunning)
        startWorker();
    emit changed();
    pumpPreview();
}

void Controller::closeSource() {
    if (opening_ || frameCount_ == 0)
        return;
    resetSource();
    closePending_ = true;
    actionError_.clear();
    emit changed();
    pumpPreview();
}

void Controller::resetSource() {
    ++previewVersion_;
    dirty_ = false;
    rendering_ = false;
    inputPath_.clear();
    outputPath_.clear();
    autoLog_.clear();
    autoError_.clear();
    frameCount_ = 0;
    frame_ = 1;
    frameName_.clear();
    image_.clear();
    previewError_.clear();
    lensModel_.clear();
    lensSupported_ = false;
    provider_.setImage({});
}

void Controller::requestPreview() {
    ++previewVersion_;
    previewError_.clear();
    dirty_ = frameCount_ != 0;
    if (dirty_ && selectedColor() == "auto-log" && autoLog_.isEmpty()) {
        dirty_ = false;
        previewError_ = tr("无法自动选择Log。请选择明确的色彩配置。\n%1").arg(autoError_);
    }
    rendering_ = dirty_;
    emit changed();
    pumpPreview();
}

void Controller::selectFrame(int frame) {
    if (frame >= 1 && frame <= frameCount_ && frame != frame_) {
        frame_ = frame;
        requestPreview();
    }
}

void Controller::selectWhiteBalance(int index) {
    if ((index == 0 || index == 1) && index != whiteBalanceIndex_) {
        whiteBalanceIndex_ = index;
        requestPreview();
    }
}

void Controller::selectColor(int index) {
    if (index >= 0 && index < colorIds_.size() && index != colorIndex_) {
        colorIndex_ = index;
        requestPreview();
    }
}

void Controller::setLensEnabled(bool enabled) {
    if (!lensModel_.isEmpty() && enabled != lensEnabled_) {
        lensEnabled_ = enabled;
        requestPreview();
    }
}

void Controller::setFps(const QString &fps) {
    if (fps == fps_)
        return;
    fps_ = fps;
    emit changed();
}

QString Controller::selectedColor() const {
    return colorIds_.value(colorIndex_);
}

QVariantList Controller::colors() const {
    QVariantList result;
    for (const auto &id : colorIds_) {
        if (id == "auto-log") {
            if (inputPath_.isEmpty())
                result.append(tr("自动选择Log"));
            else
                result.append(tr("自动选择Log（%1）").arg(autoLog_.isEmpty() ? tr("未匹配") : colorName(autoLog_, true)));
        } else {
            result.append(colorName(id));
        }
    }
    return result;
}

std::optional<QJsonValue> Controller::profileFps() const {
    static const QRegularExpression pattern(R"(^([0-9]{1,4})(?:\.([0-9]{1,9}))?$)");
    const auto match = pattern.match(fps_);
    if (!match.hasMatch())
        return std::nullopt;
    const auto whole = match.captured(1).toLongLong();
    const auto fraction = match.captured(2);
    qint64 denominator = 1;
    for (qsizetype i = 0; i < fraction.size(); ++i)
        denominator *= 10;
    const qint64 numerator = whole * denominator + fraction.toLongLong();
    if (numerator == 0 || numerator > 1000 * denominator)
        return std::nullopt;
    const qint64 divisor = std::gcd(numerator, denominator);
    if (divisor == denominator)
        return QJsonValue(static_cast<int>(numerator / denominator));
    return QJsonValue(QString("%1/%2").arg(numerator / divisor).arg(denominator / divisor));
}

bool Controller::fpsValid() const {
    return profileFps().has_value();
}

bool Controller::canExport() const {
    const auto fps = profileFps();
    if (!ready() || inputPath_.isEmpty() || !fps || (selectedColor() == "auto-log" && autoLog_.isEmpty()))
        return false;
    const auto profile = exportProfile(*fps);
    return std::ranges::none_of(jobs_, [this, &profile](const Job &job) {
        return (job.state == JobState::Waiting || job.state == JobState::Running)
            && job.input == inputPath_ && job.profile == profile;
    });
}

QJsonObject Controller::exportProfile(const QJsonValue &fps) const {
    auto profile = baseProfile_;
    profile.insert("fps", fps);
    profile.insert("white_balance", whiteBalanceIndex_ == 0 ? "first-frame" : "per-frame");
    profile.insert("lens_correction", lensEnabled_ ? "auto" : "off");
    profile.insert("output_color", selectedColor());
    profile.insert("dng_linear_frame", nativePath_);
    profile.insert("exiftool", exiftool_);
    profile.insert("ffmpeg", ffmpeg_);
    profile.insert("ffprobe", ffprobe_);
    if (!converter_.isEmpty())
        profile.insert("converter", QFileInfo(converter_).absoluteFilePath());
    return profile;
}

QString Controller::availableOutputPath() const {
    const QFileInfo base(outputPath_);
    QString path = outputPath_;
    for (int number = 2; ; ++number) {
        const QFileInfo video(path);
        const QFileInfo log(video.dir().filePath(video.completeBaseName() + ".log"));
        if (!video.exists() && !video.isSymLink() && !log.exists() && !log.isSymLink()
            && std::ranges::none_of(jobs_, [&path](const Job &job) {
                return job.output == path && (job.state == JobState::Waiting || job.state == JobState::Running);
            }))
            return path;
        path = base.dir().filePath(QString("%1-%2.%3").arg(base.completeBaseName()).arg(number).arg(base.suffix()));
    }
}

void Controller::enqueueExport() {
    if (!canExport())
        return;
    jobs_.append(Job{++nextJobId_, inputPath_, availableOutputPath(), exportProfile(*profileFps()), JobState::Waiting, {}});
    actionError_.clear();
    emit changed();
    runNextExport();
}

void Controller::runNextExport() {
    if (activeJob_ || shuttingDown_ || encoder_.state() != QProcess::NotRunning)
        return;
    for (auto &job : jobs_) {
        if (job.state != JobState::Waiting)
            continue;
        activeJob_ = job.id;
        job.state = JobState::Running;
        encoderError_.clear();
        exportReplies_.clear();
        exportPercent_ = 0;
        exportPhase_ = tr("准备导出");
        emit progressChanged();
        const auto profilePath = QDir(scratch_.path()).filePath(QString("job-%1.json").arg(job.id));
        QSaveFile profileFile(profilePath);
        const auto data = QJsonDocument(job.profile).toJson();
        if (!profileFile.open(QIODevice::WriteOnly) || profileFile.write(data) != data.size()
            || !profileFile.commit()) {
            finishExport(false, tr("无法写入导出配置。"));
            return;
        }
        encoder_.start(cliPath_, {"--input", job.input, "--profile", profilePath, "--output", job.output});
        emit changed();
        return;
    }
}

void Controller::readExportProgress() {
    exportReplies_.append(encoder_.readAllStandardOutput());
    static const QRegularExpression message(R"(^rawtl-progress ([0-9]{1,2}) (convert|deflicker|exposure|encode|verify)$)");
    static const QHash<QString, QString> phases = {
        {"convert", tr("Adobe DNG 转换")}, {"deflicker", tr("计算去闪")},
        {"exposure", tr("计算参考曝光")}, {"encode", tr("显影与编码")}, {"verify", tr("检查输出")},
    };
    qsizetype start = 0;
    for (qsizetype end = exportReplies_.indexOf('\n'); end >= 0; end = exportReplies_.indexOf('\n', start)) {
        const auto line = QString::fromUtf8(QByteArrayView(exportReplies_).sliced(start, end - start)).trimmed();
        start = end + 1;
        const auto match = message.match(line);
        if (!match.hasMatch())
            continue;
        const int percent = match.captured(1).toInt();
        const QString phase = phases.value(match.captured(2));
        if (percent >= exportPercent_ && (percent != exportPercent_ || phase != exportPhase_)) {
            exportPercent_ = percent;
            exportPhase_ = phase;
            emit progressChanged();
        }
    }
    exportReplies_.remove(0, start);
}

void Controller::finishExport(bool success, const QString &error) {
    if (shuttingDown_ || activeJob_ == 0)
        return;
    const int id = std::exchange(activeJob_, 0);
    for (auto &job : jobs_) {
        if (job.id != id)
            continue;
        const bool published = success && QFileInfo(job.output).isFile();
        job.state = published ? JobState::Done : JobState::Failed;
        if (!published)
            job.error = success ? tr("导出完成，但未找到输出文件。") : error;
        break;
    }
    QFile::remove(QDir(scratch_.path()).filePath(QString("job-%1.json").arg(id)));
    emit changed();
    QTimer::singleShot(0, this, &Controller::runNextExport);
}

int Controller::outstanding() const {
    return static_cast<int>(std::ranges::count_if(jobs_, [](const Job &job) {
        return job.state == JobState::Waiting || job.state == JobState::Running;
    }));
}

bool Controller::canClear() const {
    return std::ranges::any_of(jobs_, [](const Job &job) {
        return job.state == JobState::Done || job.state == JobState::Failed;
    });
}

QVariantList Controller::jobs() const {
    QVariantList result;
    for (const auto &job : jobs_) {
        QString status;
        switch (job.state) {
        case JobState::Waiting: status = tr("等待"); break;
        case JobState::Running: status = tr("导出中"); break;
        case JobState::Done: status = tr("已完成"); break;
        case JobState::Failed: status = tr("失败"); break;
        }
        result.append(QVariantMap{
            {"jobId", job.id}, {"path", job.input}, {"status", status}, {"error", job.error},
            {"finished", job.state == JobState::Done}, {"running", job.state == JobState::Running},
        });
    }
    return result;
}

void Controller::clearFinished() {
    jobs_.removeIf([](const Job &job) {
        return job.state == JobState::Done || job.state == JobState::Failed;
    });
    emit changed();
}

void Controller::openOutputFolder(int id) {
    for (const auto &job : jobs_) {
        if (job.id != id || job.state != JobState::Done)
            continue;
        const auto folder = QFileInfo(job.output).absolutePath();
        if (!QFileInfo(folder).isDir() || !QDesktopServices::openUrl(QUrl::fromLocalFile(desktopPath(folder))))
            setError(tr("无法打开导出所在文件夹：%1").arg(QDir::toNativeSeparators(folder)));
        return;
    }
}

void Controller::dismissError() {
    setError({});
}

void Controller::setError(const QString &error) {
    actionError_ = error;
    emit changed();
}
