use failure::{
    bail,
    ensure,
    Fallible,
};
use std::{
    cell::RefCell,
    fmt::Debug,
    str::FromStr,
};

use iota_streams_core::{
    prng,
    psk,
    sponge::spongos,
    tbits::{
        trinary,
        word::{
            IntTbitWord,
            RngTbitWord,
            SpongosTbitWord,
            StringTbitWord,
        },
        Tbits,
    },
};
use iota_streams_core_mss::signature::mss;
use iota_streams_core_ntru::key_encapsulation::ntru;

use iota_streams_app::message::{
    header::Header,
    *,
};
use iota_streams_protobuf3::types::*;

use super::*;
use crate::message::*;

use serde::{Serialize, Deserialize};

/// Generic Channel Subscriber type parametrised by the type of links, link store and
/// link generator.
///
/// `Link` type defines, well, type of links used by transport layer to identify messages.
/// For example, for HTTP it can be URL, and for the Tangle it's a pair `address`+`tag`
/// transaction fields (see `TangleAddress` type). `Link` type must implement `HasLink`
/// and `AbsorbExternalFallback` traits.
///
/// `Store` type abstracts over different kinds of link storages. Link storage is simply
/// a map from link to a spongos state and associated info corresponding to the message
/// referred by the link. `Store` must implement `LinkStore<Link::Rel>` trait as
/// it's only allowed to link messages within the same channel instance.
///
/// `LinkGen` is a helper tool for deriving links for new messages. It maintains a
/// mutable state and can derive link pseudorandomly.
#[derive(Serialize, Deserialize)]
pub struct SubscriberT<TW, F, P, Link, Store, LinkGen>
where
    P: mss::Parameters<TW>,
{
    /// PRNG used for NTRU, Spongos key generation, etc.
    prng: prng::Prng<TW, P::PrngG>,

    /// Own optional pre-shared key.
    pub(crate) opt_psk: Option<(psk::PskId<TW>, psk::Psk<TW>)>,

    /// Own optional NTRU key pair.
    pub(crate) opt_ntru: Option<(ntru::PrivateKey<TW, F>, ntru::PublicKey<TW, F>)>,

    /// Address of the Announce message or nothing if Subscriber is not registered to
    /// the channel instance.
    pub(crate) appinst: Option<Link>,

    /// Author's MSS public key, or nothing if Subscriber is not registered to
    /// the channel instance.
    //TODO: Store also Author's old MSS public keys?
    pub(crate) author_mss_pk: Option<mss::PublicKey<TW, P>>,

    /// Author's NTRU public key or nothing if Author has no NTRU key pair.
    pub(crate) author_ntru_pk: Option<ntru::PublicKey<TW, F>>,

    /// Link store.
    store: RefCell<Store>,

    /// Link generator.
    pub(crate) link_gen: LinkGen,
}

impl<TW, F, P, Link, Store, LinkGen> SubscriberT<TW, F, P, Link, Store, LinkGen>
where
    TW: RngTbitWord + IntTbitWord + StringTbitWord + SpongosTbitWord + trinary::TritWord,
    F: PRP<TW> + Clone + Default,
    P: mss::Parameters<TW>,
    Link: HasLink + AbsorbExternalFallback<TW, F> + Default + Clone + Eq,
    <Link as HasLink>::Base: Eq + Debug,
    <Link as HasLink>::Rel: Eq + Debug + Default + SkipFallback<TW, F>,
    Store: LinkStore<TW, F, <Link as HasLink>::Rel>,
    LinkGen: ChannelLinkGenerator<TW, P, Link>,
{
    /// Create a new Subscriber and optionally generate NTRU key pair.
    pub fn gen(
        store: Store,
        link_gen: LinkGen,
        prng: prng::Prng<TW, P::PrngG>,
        nonce: &Tbits<TW>,
        with_ntru: bool,
    ) -> Self {
        let opt_ntru = if with_ntru {
            //TODO: Derive ntru nonce.
            let ntru_nonce = &Tbits::<TW>::from_str("NTRUNONCE").unwrap() + nonce;
            let key_pair = ntru::gen_keypair::<TW, F, P::PrngG>(&prng, ntru_nonce.slice());
            Some(key_pair)
        } else {
            None
        };

        Self {
            prng: prng,
            opt_ntru: opt_ntru,
            opt_psk: None,

            appinst: None,
            author_mss_pk: None,
            author_ntru_pk: None,

            store: RefCell::new(store),
            link_gen: link_gen,
        }
    }

    fn ensure_appinst<'a>(&self, preparsed: &PreparsedMessage<'a, TW, F, Link>) -> Fallible<()> {
        ensure!(self.appinst.is_some(), "Subscriber is not subscribed to a channel.");
        ensure!(
            self.appinst.as_ref().unwrap().base() == preparsed.header.link.base(),
            "Bad message application instance."
        );
        Ok(())
    }

    fn do_prepare_keyload<'a, Psks, NtruPks>(
        &'a self,
        header: Header<TW, Link>,
        link_to: &'a <Link as HasLink>::Rel,
        psks: Psks,
        ntru_pks: NtruPks,
    ) -> Fallible<PreparedMessage<'a, TW, F, Link, Store, keyload::ContentWrap<'a, TW, F, P::PrngG, Link, Psks, NtruPks>>>
    where
        Psks: Clone + ExactSizeIterator<Item = psk::IPsk<'a, TW>>,
        NtruPks: Clone + ExactSizeIterator<Item = ntru::INtruPk<'a, TW, F>>,
    {
        let nonce = NTrytes(prng::random_nonce(spongos::Spongos::<TW, F>::NONCE_SIZE));
        let key = NTrytes(prng::random_key(spongos::Spongos::<TW, F>::KEY_SIZE));
        let content = keyload::ContentWrap {
            link: link_to,
            nonce: nonce,
            key: key,
            psks: psks,
            prng: &self.prng,
            ntru_pks: ntru_pks,
            _phantom: std::marker::PhantomData,
        };
        Ok(PreparedMessage::new(self.store.borrow(), header, content))
    }

    pub fn prepare_keyload<'a>(
        &'a mut self,
        link_to: &'a <Link as HasLink>::Rel,
    ) -> Fallible<
        PreparedMessage<
            'a,
            TW,
            F,
            Link,
            Store,
            keyload::ContentWrap<
                'a,
                TW,
                F,
                P::PrngG,
                Link,
                std::option::IntoIter<psk::IPsk<'a, TW>>,
                std::option::IntoIter<ntru::INtruPk<'a, TW, F>>,
            >,
        >,
    > {
        let header = self.link_gen.header_from(link_to, keyload::TYPE);
        self.do_prepare_keyload(
            header,
            link_to,
            self.opt_psk.as_ref().map(|(pskid, psk)| (pskid, psk)).into_iter(),
            self.author_ntru_pk.as_ref().into_iter(),
        )
    }

    /// Create keyload message with a new session key shared with recipients
    /// identified by pre-shared key IDs and by NTRU public key IDs.
    pub fn share_keyload(
        &mut self,
        link_to: &<Link as HasLink>::Rel,
        info: <Store as LinkStore<TW, F, <Link as HasLink>::Rel>>::Info,
    ) -> Fallible<TbinaryMessage<TW, F, Link>> {
        let wrapped = self.prepare_keyload(link_to)?.wrap()?;
        wrapped.commit(self.store.borrow_mut(), info)
    }

    /// Prepare TaggedPacket message.
    pub fn prepare_tagged_packet<'a>(
        &'a mut self,
        link_to: &'a <Link as HasLink>::Rel,
        public_payload: &'a Trytes<TW>,
        masked_payload: &'a Trytes<TW>,
    ) -> Fallible<PreparedMessage<'a, TW, F, Link, Store, tagged_packet::ContentWrap<'a, TW, F, Link>>> {
        let header = self.link_gen.header_from(link_to, tagged_packet::TYPE);
        let content = tagged_packet::ContentWrap {
            link: link_to,
            public_payload: public_payload,
            masked_payload: masked_payload,
            _phantom: std::marker::PhantomData,
        };
        Ok(PreparedMessage::new(self.store.borrow(), header, content))
    }

    /// Create a tagged (ie. MACed) message with public and masked payload.
    /// Tagged messages must be linked to a secret spongos state, ie. keyload or a message linked to keyload.
    pub fn tag_packet(
        &mut self,
        link_to: &<Link as HasLink>::Rel,
        public_payload: &Trytes<TW>,
        masked_payload: &Trytes<TW>,
        info: <Store as LinkStore<TW, F, <Link as HasLink>::Rel>>::Info,
    ) -> Fallible<TbinaryMessage<TW, F, Link>> {
        let wrapped = self
            .prepare_tagged_packet(link_to, public_payload, masked_payload)?
            .wrap()?;
        wrapped.commit(self.store.borrow_mut(), info)
    }

    /// Prepare Subscribe message.
    pub fn prepare_subscribe<'a>(
        &'a mut self,
        link_to: &'a <Link as HasLink>::Rel,
    ) -> Fallible<PreparedMessage<'a, TW, F, Link, Store, subscribe::ContentWrap<'a, TW, F, P::PrngG, Link>>> {
        if let Some(author_ntru_pk) = &self.author_ntru_pk {
            if let Some((_, own_ntru_pk)) = &self.opt_ntru {
                let header = self.link_gen.header_from(link_to, subscribe::TYPE);
                let nonce = NTrytes(prng::random_nonce(spongos::Spongos::<TW, F>::NONCE_SIZE));
                let unsubscribe_key = NTrytes(prng::random_key(spongos::Spongos::<TW, F>::KEY_SIZE));
                let content = subscribe::ContentWrap {
                    link: link_to,
                    nonce,
                    unsubscribe_key,
                    subscriber_ntru_pk: own_ntru_pk,
                    author_ntru_pk: author_ntru_pk,
                    prng: &self.prng,
                    _phantom: std::marker::PhantomData,
                };
                Ok(PreparedMessage::new(self.store.borrow(), header, content))
            } else {
                bail!("Subscriber doesn't have own NTRU key pair.");
            }
        } else {
            bail!("Subscriber doesn't have channel Author's NTRU public key.");
        }
    }

    /// Subscribe to the channel.
    pub fn subscribe(
        &mut self,
        link_to: &<Link as HasLink>::Rel,
        info: <Store as LinkStore<TW, F, <Link as HasLink>::Rel>>::Info,
    ) -> Fallible<TbinaryMessage<TW, F, Link>> {
        let wrapped = self.prepare_subscribe(link_to)?.wrap()?;
        wrapped.commit(self.store.borrow_mut(), info)
    }

    /// Prepare Unsubscribe message.
    pub fn prepare_unsubscribe<'a>(
        &'a mut self,
        link_to: &'a <Link as HasLink>::Rel,
    ) -> Fallible<PreparedMessage<'a, TW, F, Link, Store, unsubscribe::ContentWrap<'a, TW, F, Link>>> {
        let header = self.link_gen.header_from(link_to, unsubscribe::TYPE);
        let content = unsubscribe::ContentWrap {
            link: link_to,
            _phantom: std::marker::PhantomData,
        };
        Ok(PreparedMessage::new(self.store.borrow(), header, content))
    }

    /// Unsubscribe from the channel.
    pub fn unsubscribe(
        &mut self,
        link_to: &<Link as HasLink>::Rel,
        info: <Store as LinkStore<TW, F, <Link as HasLink>::Rel>>::Info,
    ) -> Fallible<TbinaryMessage<TW, F, Link>> {
        let wrapped = self.prepare_unsubscribe(link_to)?.wrap()?;
        wrapped.commit(self.store.borrow_mut(), info)
    }

    pub fn unwrap_announcement<'a>(
        &self,
        preparsed: PreparsedMessage<'a, TW, F, Link>,
    ) -> Fallible<UnwrappedMessage<TW, F, Link, announce::ContentUnwrap<TW, F, P>>> {
        if let Some(appinst) = &self.appinst {
            ensure!(
                appinst == &preparsed.header.link,
                "Got Announce with address {:?}, but already registered to a channel {:?}",
                preparsed.header.link.base(),
                appinst.base()
            );
        }

        let content = announce::ContentUnwrap::<TW, F, P>::default();
        preparsed.unwrap(&*self.store.borrow(), content)
    }

    /// Bind Subscriber (or anonymously subscribe) to the channel announced
    /// in the message.
    pub fn handle_announcement<'a>(
        &mut self,
        preparsed: PreparsedMessage<'a, TW, F, Link>,
        info: <Store as LinkStore<TW, F, <Link as HasLink>::Rel>>::Info,
    ) -> Fallible<()> {
        let unwrapped = self.unwrap_announcement(preparsed)?;
        let link = unwrapped.link.clone();
        let content = unwrapped.commit(self.store.borrow_mut(), info)?;
        //TODO: check commit after message is done / before joined

        //TODO: Verify trust to Author's MSS public key?
        // At the moment the Author is trusted unconditionally.

        //TODO: Verify appinst (address) == MSS public key.
        // At the moment the Author is free to choose any address, not tied to MSS PK.

        self.appinst = Some(link);
        self.author_mss_pk = Some(content.mss_pk);
        self.author_ntru_pk = content.ntru_pk;
        Ok(())
    }

    pub fn unwrap_change_key<'a, 'b>(
        &'b self,
        preparsed: PreparsedMessage<'a, TW, F, Link>,
    ) -> Fallible<UnwrappedMessage<TW, F, Link, change_key::ContentUnwrap<'b, TW, P, Link>>> {
        self.ensure_appinst(&preparsed)?;
        let mss_linked_pk = self.author_mss_pk.as_ref().unwrap();
        let content = change_key::ContentUnwrap::new(mss_linked_pk);
        preparsed.unwrap(&*self.store.borrow(), content)
    }

    /// Verify new Author's MSS public key and update Author's MSS public key.
    pub fn handle_change_key<'a>(
        &mut self,
        preparsed: PreparsedMessage<'a, TW, F, Link>,
        info: <Store as LinkStore<TW, F, <Link as HasLink>::Rel>>::Info,
    ) -> Fallible<()> {
        ensure!(self.author_mss_pk.is_some(), "No Author's MSS public key found.");
        let content = self
            .unwrap_change_key(preparsed)?
            .commit(self.store.borrow_mut(), info)?;
        self.author_mss_pk = Some(content.mss_pk);
        Ok(())
    }

    fn lookup_psk<'b>(&'b self, pskid: &psk::PskId<TW>) -> Option<&'b psk::Psk<TW>> {
        self.opt_psk.as_ref().map_or(
            None,
            |(own_pskid, own_psk)| {
                if pskid == own_pskid {
                    Some(own_psk)
                } else {
                    None
                }
            },
        )
    }

    fn lookup_ntru_sk<'b>(&'b self, ntru_pkid: &ntru::Pkid<TW>) -> Option<&'b ntru::PrivateKey<TW, F>> {
        self.opt_ntru.as_ref().map_or(None, |(own_ntru_sk, own_ntru_pk)| {
            if own_ntru_pk.cmp_pkid(ntru_pkid) {
                Some(own_ntru_sk)
            } else {
                None
            }
        })
    }

    pub fn unwrap_keyload<'a, 'b>(
        &'b self,
        preparsed: PreparsedMessage<'a, TW, F, Link>,
    ) -> Fallible<
        UnwrappedMessage<
            TW,
            F,
            Link,
            keyload::ContentUnwrap<
                'b,
                TW,
                F,
                Link,
                Self,
                for<'c> fn(&'c Self, &psk::PskId<TW>) -> Option<&'c psk::Psk<TW>>,
                for<'c> fn(&'c Self, &ntru::Pkid<TW>) -> Option<&'c ntru::PrivateKey<TW, F>>,
            >,
        >,
    > {
        self.ensure_appinst(&preparsed)?;
        let content = keyload::ContentUnwrap::<
            'b,
            TW,
            F,
            Link,
            Self,
            for<'c> fn(&'c Self, &psk::PskId<TW>) -> Option<&'c psk::Psk<TW>>,
            for<'c> fn(&'c Self, &ntru::Pkid<TW>) -> Option<&'c ntru::PrivateKey<TW, F>>,
        >::new(self, Self::lookup_psk, Self::lookup_ntru_sk);
        preparsed.unwrap(&*self.store.borrow(), content)
    }

    /// Try unwrapping session key from keyload using Subscriber's pre-shared key or NTRU private key (if any).
    pub fn handle_keyload<'a>(
        &mut self,
        preparsed: PreparsedMessage<'a, TW, F, Link>,
        info: <Store as LinkStore<TW, F, <Link as HasLink>::Rel>>::Info,
    ) -> Fallible<()> {
        let _content = self.unwrap_keyload(preparsed)?.commit(self.store.borrow_mut(), info)?;
        // Unwrapped nonce and key in content are not used explicitly.
        // The resulting spongos state is joined into a protected message state.
        Ok(())
    }

    pub fn unwrap_signed_packet<'a>(
        &self,
        preparsed: PreparsedMessage<'a, TW, F, Link>,
    ) -> Fallible<UnwrappedMessage<TW, F, Link, signed_packet::ContentUnwrap<TW, F, P, Link>>> {
        self.ensure_appinst(&preparsed)?;
        ensure!(
            self.author_mss_pk.is_some(),
            "No Author's MSS public key found, can't verify signature."
        );
        let content = signed_packet::ContentUnwrap::new();
        preparsed.unwrap(&*self.store.borrow(), content)
    }

    /// Verify new Author's MSS public key and update Author's MSS public key.
    pub fn handle_signed_packet<'a>(
        &mut self,
        preparsed: PreparsedMessage<'a, TW, F, Link>,
        info: <Store as LinkStore<TW, F, <Link as HasLink>::Rel>>::Info,
    ) -> Fallible<(Trytes<TW>, Trytes<TW>)> {
        ensure!(self.author_mss_pk.is_some(), "No Author's MSS public key found.");
        let content = self
            .unwrap_signed_packet(preparsed)?
            .commit(self.store.borrow_mut(), info)?;
        ensure!(
            self.author_mss_pk
                .as_ref()
                .map_or(false, |mss_pk| *mss_pk == content.mss_pk),
            "Bad signed packet signature."
        );
        Ok((content.public_payload, content.masked_payload))
    }

    pub fn unwrap_tagged_packet<'a>(
        &self,
        preparsed: PreparsedMessage<'a, TW, F, Link>,
    ) -> Fallible<UnwrappedMessage<TW, F, Link, tagged_packet::ContentUnwrap<TW, F, Link>>> {
        self.ensure_appinst(&preparsed)?;
        let content = tagged_packet::ContentUnwrap::new();
        preparsed.unwrap(&*self.store.borrow(), content)
    }

    /// Get public payload, decrypt masked payload and verify MAC.
    pub fn handle_tagged_packet<'a>(
        &mut self,
        preparsed: PreparsedMessage<'a, TW, F, Link>,
        info: <Store as LinkStore<TW, F, <Link as HasLink>::Rel>>::Info,
    ) -> Fallible<(Trytes<TW>, Trytes<TW>)> {
        let content = self
            .unwrap_tagged_packet(preparsed)?
            .commit(self.store.borrow_mut(), info)?;
        Ok((content.public_payload, content.masked_payload))
    }

    /*
       /// Unwrap message.
       pub fn handle_msg(
           &mut self,
           msg: &TbinaryMessage<TW, F, Link>,
           info: <Store as LinkStore<TW, F, <Link as HasLink>::Rel>>::Info,
       ) -> Fallible<()> {
           if self.appinst.is_some() {
               ensure!(
                   self.appinst.as_ref().unwrap().base() == msg.link().base(),
                   "Bad message application instance."
               );
           }

           let preparsed = msg.parse_header()?;

           if preparsed.check_content_type(announce::TYPE) {
               self.handle_announcement(preparsed, info)?;
               Ok(())
           } else if preparsed.check_content_type(change_key::TYPE) {
               self.handle_change_key(preparsed, info)?;
               Ok(())
           } else if preparsed.check_content_type(signed_packet::TYPE) {
               self.handle_signed_packet(preparsed, info)?;
               Ok(())
           } else if preparsed.check_content_type(tagged_packet::TYPE) {
               self.handle_tagged_packet(preparsed, info)?;
               Ok(())
           } else
           /*
           if preparsed.check_content_type(keyload::TYPE) {
               self.handle_keyload(preparsed, info)?;
               Ok(())
           } else
            */
           {
               bail!("Unsupported content type: '{}'.", preparsed.content_type())
           }
       }
    */
}
