#include "cirroveclient.h"

#include <KOverlayIconPlugin>
#include <QHash>
#include <QJsonArray>
#include <QJsonDocument>
#include <QLocalSocket>
#include <QPointer>
#include <QSet>
#include <QTimer>
#include <QUrl>
#include <algorithm>

using namespace CirroveDolphin;

class CirroveOverlayIconPlugin final : public KOverlayIconPlugin {
  Q_OBJECT
  Q_PLUGIN_METADATA(IID "org.kde.overlayicon.cirrove")

public:
  CirroveOverlayIconPlugin() {
    reconnect_.setSingleShot(true);
    connect(&reconnect_, &QTimer::timeout, this,
            &CirroveOverlayIconPlugin::subscribe);
    subscribe();
  }

  QStringList getOverlays(const QUrl &url) override {
    if (!url.isLocalFile()) {
      return {};
    }
    const auto key = url.toString(QUrl::FullyEncoded);
    if (overlays_.contains(key)) {
      return overlays_.value(key);
    }
    remember(url);
    pending_.insert(key);
    dispatch();
    return {};
  }

private:
  void subscribe() {
    priming_ = true;
    streamBytes_.clear();
    if (auto *old = stream_.data()) {
      old->disconnect(this);
      old->abort();
      old->deleteLater();
    }
    auto *socket = new QLocalSocket(this);
    stream_ = socket;
    connect(socket, &QLocalSocket::connected, this, [this, socket] {
      reconnect_.stop();
      socket->write("subscribe\n");
      socket->flush();
    });
    connect(socket, &QLocalSocket::readyRead, this, [this, socket] {
      streamBytes_ += socket->readAll();
      while (true) {
        const auto newline = streamBytes_.indexOf('\n');
        if (newline < 0) {
          break;
        }
        const auto line = streamBytes_.left(newline);
        streamBytes_.remove(0, newline + 1);
        const auto document = QJsonDocument::fromJson(line);
        if (document.isObject()) {
          observe(document.object());
        }
      }
    });
    connect(socket, &QLocalSocket::disconnected, this,
            &CirroveOverlayIconPlugin::retry);
    connect(socket, &QLocalSocket::errorOccurred, this,
            [this](QLocalSocket::LocalSocketError) { retry(); });
    socket->connectToServer(controlSocketPath(), QIODevice::ReadWrite);
  }

  void retry() {
    if (!reconnect_.isActive()) {
      priming_ = true;
      invalidate(QString());
      reconnect_.start(reconnectDelayMs_);
      reconnectDelayMs_ = std::min(reconnectDelayMs_ * 2, 60000);
    }
  }

  void observe(const QJsonObject &event) {
    reconnectDelayMs_ = 2000;
    const auto kind = event.value(QStringLiteral("event")).toString();
    if (kind == QLatin1String("mount")) {
      const auto id = event.value(QStringLiteral("account_id")).toString();
      mounts_.removeIf(
          [&id](const Mount &mount) { return mount.accountId == id; });
      if (event.value(QStringLiteral("mounted")).toBool()) {
        mounts_.push_back(
            {id, event.value(QStringLiteral("label")).toString(),
             event.value(QStringLiteral("mount_path")).toString()});
      }
      invalidate(QString());
    } else if (kind == QLatin1String("account")) {
      const auto id = event.value(QStringLiteral("account_id")).toString();
      const auto label = event.value(QStringLiteral("label")).toString();
      const auto generation =
          event.value(QStringLiteral("kept_generation")).toInteger();
      const auto before = generations_.value(id, -1);
      generations_.insert(id, generation);
      if (!priming_ && before >= 0 && before != generation) {
        invalidate(label);
      }
    } else if (kind == QLatin1String("account_removed")) {
      const auto id = event.value(QStringLiteral("account_id")).toString();
      mounts_.removeIf(
          [&id](const Mount &mount) { return mount.accountId == id; });
      generations_.remove(id);
      invalidate(QString());
    } else if (kind == QLatin1String("lagged")) {
      priming_ = true;
      invalidate(QString());
    } else if (kind == QLatin1String("ready")) {
      priming_ = false;
      dispatch();
    }
  }

  void remember(const QUrl &url) {
    const auto key = url.toString(QUrl::FullyEncoded);
    shown_.remove(key);
    shown_.insert(key, url);
    while (shown_.size() > 512) {
      const auto oldest = shown_.begin().key();
      shown_.erase(shown_.begin());
      overlays_.remove(oldest);
      labels_.remove(oldest);
      pending_.remove(oldest);
      revisions_.remove(oldest);
    }
  }

  void dispatch() {
    if (priming_) {
      return;
    }
    const auto waiting = pending_;
    pending_.clear();
    for (const auto &key : waiting) {
      if (inFlight_.contains(key)) {
        pending_.insert(key);
        continue;
      }
      const auto url = shown_.value(key);
      const auto found = locate(mounts_, url.toLocalFile());
      if (!found) {
        overlays_.insert(key, {});
        continue;
      }
      labels_.insert(key, found->label);
      inFlight_.insert(key);
      const auto revision = revisions_.value(key, 0);
      QJsonArray paths;
      paths.append(found->relative);
      requestAsync(
          this, QStringLiteral("paths"),
          {{QStringLiteral("label"), found->label},
           {QStringLiteral("paths"), paths}},
          [this, key, url, revision](std::optional<QJsonObject> reply) {
            inFlight_.remove(key);
            if (!shown_.contains(key)) {
              return;
            }
            if (revision != revisions_.value(key, 0)) {
              dispatch();
              return;
            }
            if (!reply) {
              overlays_.remove(key);
              return;
            }
            QStringList value;
            const auto states = statesFromReply(*reply);
            if (!states.isEmpty()) {
              const auto overlay = overlayFor(states.first());
              if (!overlay.isEmpty()) {
                value.push_back(overlay);
              }
            }
            overlays_.insert(key, value);
            Q_EMIT overlaysChanged(url, value);
          });
    }
  }

  void invalidate(const QString &label) {
    for (auto it = shown_.cbegin(); it != shown_.cend(); ++it) {
      if (!label.isEmpty() && labels_.value(it.key()) != label) {
        continue;
      }
      overlays_.remove(it.key());
      revisions_.insert(it.key(), revisions_.value(it.key(), 0) + 1);
      pending_.insert(it.key());
      Q_EMIT overlaysChanged(it.value(), {});
    }
    dispatch();
  }

  QPointer<QLocalSocket> stream_;
  QByteArray streamBytes_;
  QTimer reconnect_;
  int reconnectDelayMs_ = 2000;
  bool priming_ = true;
  QList<Mount> mounts_;
  QHash<QString, qint64> generations_;
  QHash<QString, QUrl> shown_;
  QHash<QString, QStringList> overlays_;
  QHash<QString, QString> labels_;
  QHash<QString, qint64> revisions_;
  QSet<QString> pending_;
  QSet<QString> inFlight_;
};

#include "overlayplugin.moc"
