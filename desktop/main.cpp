#include "controller.h"

#include <QGuiApplication>
#include <QQmlApplicationEngine>
#include <QQuickStyle>

int main(int argc, char **argv) {
    QGuiApplication app(argc, argv);
    app.setApplicationName("RAW Timelapse");
    QQuickStyle::setStyle("Fusion");
    auto *preview = new PreviewProvider;
    Controller controller(*preview);
    QQmlApplicationEngine engine;
    engine.addImageProvider("preview", preview);
    engine.setInitialProperties({{"workspace", QVariant::fromValue(&controller)}});
    QObject::connect(&engine, &QQmlApplicationEngine::objectCreationFailed,
                     &app, [] { QCoreApplication::exit(1); }, Qt::QueuedConnection);
    engine.loadFromModule("RawTimelapse", "Main");
    return app.exec();
}
