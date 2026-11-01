//! Deviation: what the client may send while the world is paused, which 1.12 never asks. The fork
//! handles a movement, spell, combat or interaction packet inside its map update, so one sent while
//! paused would wait there and run at the resume, with its siblings, all at once. They stop here,
//! at the one point every command passes; the match has no wildcard, so a new command must declare
//! itself. Cast attempts refuse earlier, ahead of any local state (`crate::spell`).
//!
//! The rest passes. Queries and chat run on the server's world thread. A bag move waits for the
//! resume, which is safe, where dropping it could leave its item locked.

use super::ClientCommand;

/// Whether `cmd` may go out while the world is paused.
pub(super) fn paused_allows(cmd: &ClientCommand) -> bool {
    match cmd {
        ClientCommand::Move { .. }
        | ClientCommand::MoveTimeSkipped { .. }
        | ClientCommand::UseItem { .. }
        | ClientCommand::OpenItem { .. }
        | ClientCommand::Emote { .. }
        | ClientCommand::TextEmote { .. }
        | ClientCommand::CastSpell { .. }
        | ClientCommand::CastSpellCorpse { .. }
        | ClientCommand::CastSpellAtDest { .. }
        | ClientCommand::CastSpellAtSource { .. }
        | ClientCommand::CancelAura { .. }
        | ClientCommand::PetAction { .. }
        | ClientCommand::PetCastSpell { .. }
        | ClientCommand::PetStopAttack { .. }
        | ClientCommand::PetCancelAura { .. }
        | ClientCommand::AttackSwing { .. }
        | ClientCommand::AttackStop
        | ClientCommand::CancelAutoRepeat
        | ClientCommand::CancelCast { .. }
        | ClientCommand::CancelChannelling { .. }
        | ClientCommand::SetSheathed { .. }
        | ClientCommand::StandStateChange { .. }
        | ClientCommand::MountSpecial
        | ClientCommand::GossipHello { .. }
        | ClientCommand::GameObjUse { .. }
        | ClientCommand::CastSpellGameObject { .. }
        | ClientCommand::CastSpellItem { .. }
        | ClientCommand::CastSpellTradeItem { .. }
        | ClientCommand::Loot { .. }
        | ClientCommand::QuestgiverHello { .. }
        | ClientCommand::RepopRequest
        | ClientCommand::ReclaimCorpse { .. }
        | ClientCommand::TeleportCheat { .. }
        | ClientCommand::TeleportToUnit { .. }
        | ClientCommand::SetRawPosition { .. }
        | ClientCommand::WorldTeleport { .. }
        | ClientCommand::ActivateTaxi { .. }
        | ClientCommand::ActivateTaxiExpress { .. } => false,

        ClientCommand::MoveSplineDone { .. }
        | ClientCommand::ForceSpeedAck { .. }
        | ClientCommand::SetActiveMover { .. }
        | ClientCommand::PartyLanded { .. }
        | ClientCommand::NotActiveMover { .. }
        | ClientCommand::FarSight { .. }
        | ClientCommand::TeleportAck { .. }
        | ClientCommand::WorldportAck
        | ClientCommand::SetSelection { .. }
        | ClientCommand::Chat { .. }
        | ClientCommand::AddonMessage { .. }
        | ClientCommand::NameQuery { .. }
        | ClientCommand::CreatureQuery { .. }
        | ClientCommand::PetNameQuery { .. }
        | ClientCommand::ItemNameQuery { .. }
        | ClientCommand::ItemQuery { .. }
        | ClientCommand::ReadItem { .. }
        | ClientCommand::WrapItem { .. }
        | ClientCommand::AutoEquipItem { .. }
        | ClientCommand::SetAmmo { .. }
        | ClientCommand::SwapInvItem { .. }
        | ClientCommand::SwapItem { .. }
        | ClientCommand::AutoStoreBagItem { .. }
        | ClientCommand::SplitItem { .. }
        | ClientCommand::DestroyItem { .. }
        | ClientCommand::SetActionButton { .. }
        | ClientCommand::SetActionBarToggles { .. }
        | ClientCommand::PetSetAction { .. }
        | ClientCommand::PetSpellAutocast { .. }
        | ClientCommand::RequestPetInfo
        | ClientCommand::PetAbandon { .. }
        | ClientCommand::PetRename { .. }
        | ClientCommand::GossipSelectOption { .. }
        | ClientCommand::NpcTextQuery { .. }
        | ClientCommand::ListInventory { .. }
        | ClientCommand::BuyItem { .. }
        | ClientCommand::BuyItemInSlot { .. }
        | ClientCommand::SellItem { .. }
        | ClientCommand::BuybackItem { .. }
        | ClientCommand::RepairItem { .. }
        | ClientCommand::GmTicketCreate { .. }
        | ClientCommand::GmTicketUpdate { .. }
        | ClientCommand::GmTicketGet
        | ClientCommand::SkillBuyStep { .. }
        | ClientCommand::LotteryQuery
        | ClientCommand::BuyLotteryTicket { .. }
        | ClientCommand::GmSurveySubmit { .. }
        | ClientCommand::GmTicketDelete
        | ClientCommand::GmTicketSystemStatus
        | ClientCommand::BinderActivate { .. }
        | ClientCommand::SummonResponse { .. }
        | ClientCommand::TalentWipeConfirm { .. }
        | ClientCommand::ForceLogout
        | ClientCommand::AreaSpiritHealerQueue { .. }
        | ClientCommand::AreaSpiritHealerQuery { .. }
        | ClientCommand::BattlefieldPort { .. }
        | ClientCommand::RequestBattlefieldScoreData
        | ClientCommand::LeaveBattlefield { .. }
        | ClientCommand::MeetingStoneJoin { .. }
        | ClientCommand::MeetingStoneLeave
        | ClientCommand::MeetingStoneStatusQuery
        | ClientCommand::RequestAccountData { .. }
        | ClientCommand::UpdateAccountData { .. }
        | ClientCommand::TutorialFlag { .. }
        | ClientCommand::TutorialClear
        | ClientCommand::TutorialReset
        | ClientCommand::BattlefieldList { .. }
        | ClientCommand::BattlemasterJoin { .. }
        | ClientCommand::RequestBattlefieldPositions
        | ClientCommand::TabardVendorActivate { .. }
        | ClientCommand::SaveGuildEmblem { .. }
        | ClientCommand::BattlemasterHello { .. }
        | ClientCommand::BattlefieldJoin { .. }
        | ClientCommand::BattlefieldStatusRequest
        | ClientCommand::PetUnlearn { .. }
        | ClientCommand::BankerActivate { .. }
        | ClientCommand::BuyBankSlot { .. }
        | ClientCommand::AutoBankItem { .. }
        | ClientCommand::AutoStoreBankItem { .. }
        | ClientCommand::TrainerList { .. }
        | ClientCommand::TrainerBuySpell { .. }
        | ClientCommand::ListStabledPets { .. }
        | ClientCommand::StablePet { .. }
        | ClientCommand::UnstablePet { .. }
        | ClientCommand::StableSwapPet { .. }
        | ClientCommand::BuyStableSlot { .. }
        | ClientCommand::LearnTalent { .. }
        | ClientCommand::UnlearnSkill { .. }
        | ClientCommand::SetFactionAtWar { .. }
        | ClientCommand::SetFactionInactive { .. }
        | ClientCommand::SetWatchedFaction { .. }
        | ClientCommand::AreaTrigger { .. }
        | ClientCommand::GameObjectQuery { .. }
        | ClientCommand::PageTextQuery { .. }
        | ClientCommand::AutostoreLootItem { .. }
        | ClientCommand::LootMoney
        | ClientCommand::LootRelease { .. }
        | ClientCommand::LootRoll { .. }
        | ClientCommand::LootMasterGive { .. }
        | ClientCommand::QuestgiverQuery { .. }
        | ClientCommand::QuestgiverAccept { .. }
        | ClientCommand::QuestgiverComplete { .. }
        | ClientCommand::QuestgiverRequestReward { .. }
        | ClientCommand::QuestgiverChooseReward { .. }
        | ClientCommand::QuestQuery { .. }
        | ClientCommand::QuestgiverStatusQuery { .. }
        | ClientCommand::QuestlogRemove { .. }
        | ClientCommand::PushQuestToParty { .. }
        | ClientCommand::QuestConfirmAccept { .. }
        | ClientCommand::QuestPushResult { .. }
        | ClientCommand::GetMailList { .. }
        | ClientCommand::SendMail { .. }
        | ClientCommand::MailTakeMoney { .. }
        | ClientCommand::MailTakeItem { .. }
        | ClientCommand::MailMarkAsRead { .. }
        | ClientCommand::MailReturnToSender { .. }
        | ClientCommand::MailDelete { .. }
        | ClientCommand::MailCreateTextItem { .. }
        | ClientCommand::ItemTextQuery { .. }
        | ClientCommand::QueryNextMailTime
        | ClientCommand::AuctionHello { .. }
        | ClientCommand::AuctionListItems { .. }
        | ClientCommand::AuctionListOwnerItems { .. }
        | ClientCommand::AuctionListBidderItems { .. }
        | ClientCommand::AuctionSellItem { .. }
        | ClientCommand::AuctionPlaceBid { .. }
        | ClientCommand::AuctionRemoveItem { .. }
        | ClientCommand::QueryTime
        | ClientCommand::Inspect { .. }
        | ClientCommand::InspectHonorStats { .. }
        | ClientCommand::InitiateTrade { .. }
        | ClientCommand::BeginTrade
        | ClientCommand::IgnoreTrade
        | ClientCommand::BusyTrade
        | ClientCommand::AcceptTrade { .. }
        | ClientCommand::UnacceptTrade
        | ClientCommand::CancelTrade
        | ClientCommand::SetTradeGold { .. }
        | ClientCommand::SetTradeItem { .. }
        | ClientCommand::ClearTradeItem { .. }
        | ClientCommand::Logout
        | ClientCommand::LogoutCancel
        | ClientCommand::JoinChannel { .. }
        | ClientCommand::LeaveChannel { .. }
        | ClientCommand::ChannelList { .. }
        | ClientCommand::RandomRoll { .. }
        | ClientCommand::ChannelOwner { .. }
        | ClientCommand::ChannelSetOwner { .. }
        | ClientCommand::ChannelPassword { .. }
        | ClientCommand::ChannelModerator { .. }
        | ClientCommand::ChannelUnmoderator { .. }
        | ClientCommand::ChannelMute { .. }
        | ClientCommand::ChannelUnmute { .. }
        | ClientCommand::ChannelInvite { .. }
        | ClientCommand::ChannelKick { .. }
        | ClientCommand::ChannelBan { .. }
        | ClientCommand::ChannelUnban { .. }
        | ClientCommand::ChannelAnnouncements { .. }
        | ClientCommand::ChannelModerate { .. }
        | ClientCommand::PlayedTime
        | ClientCommand::OpeningCinematic
        | ClientCommand::CompleteCinematic
        | ClientCommand::NextCinematicCamera
        | ClientCommand::MoveModeAck { .. }
        | ClientCommand::KnockBackAck { .. }
        | ClientCommand::CorpseQuery
        | ClientCommand::SelfRes
        | ClientCommand::SpiritHealerActivate { .. }
        | ClientCommand::ResurrectResponse { .. }
        | ClientCommand::GroupInvite { .. }
        | ClientCommand::GroupAccept
        | ClientCommand::GroupDecline
        | ClientCommand::GroupUninvite { .. }
        | ClientCommand::GroupUninviteGuid { .. }
        | ClientCommand::GroupSetLeader { .. }
        | ClientCommand::GroupLeave
        | ClientCommand::GroupRaidConvert
        | ClientCommand::RequestPartyMemberStats { .. }
        | ClientCommand::LootMethod { .. }
        | ClientCommand::SetRaidTarget { .. }
        | ClientCommand::RequestRaidTargets
        | ClientCommand::MinimapPing { .. }
        | ClientCommand::GroupChangeSubGroup { .. }
        | ClientCommand::GroupSwapSubGroup { .. }
        | ClientCommand::GroupAssistantLeader { .. }
        | ClientCommand::ReadyCheckStart
        | ClientCommand::ReadyCheckAnswer { .. }
        | ClientCommand::RequestRaidInfo
        | ClientCommand::ResetInstances
        | ClientCommand::DuelAccepted { .. }
        | ClientCommand::DuelCancelled { .. }
        | ClientCommand::FriendListRequest
        | ClientCommand::AddFriend { .. }
        | ClientCommand::SetLookingForGroup { .. }
        | ClientCommand::LfgQuery { .. }
        | ClientCommand::DelFriend { .. }
        | ClientCommand::AddIgnore { .. }
        | ClientCommand::DelIgnore { .. }
        | ClientCommand::ChatIgnored { .. }
        | ClientCommand::Who { .. }
        | ClientCommand::TogglePvp
        | ClientCommand::ToggleHelm
        | ClientCommand::ToggleCloak
        | ClientCommand::Whois { .. }
        | ClientCommand::GuildQuery { .. }
        | ClientCommand::GuildCreate { .. }
        | ClientCommand::GuildInvite { .. }
        | ClientCommand::GuildAccept
        | ClientCommand::GuildDecline
        | ClientCommand::GuildInfoRequest
        | ClientCommand::GuildRosterRequest
        | ClientCommand::GuildPromote { .. }
        | ClientCommand::GuildDemote { .. }
        | ClientCommand::GuildLeave
        | ClientCommand::GuildRemove { .. }
        | ClientCommand::GuildDisband
        | ClientCommand::GuildLeader { .. }
        | ClientCommand::GuildMotd { .. }
        | ClientCommand::GuildRank { .. }
        | ClientCommand::GuildAddRank { .. }
        | ClientCommand::GuildDelRank
        | ClientCommand::GuildSetPublicNote { .. }
        | ClientCommand::GuildSetOfficerNote { .. }
        | ClientCommand::GuildInfoText { .. }
        | ClientCommand::PetitionShowList { .. }
        | ClientCommand::PetitionBuy { .. }
        | ClientCommand::PetitionShowSignatures { .. }
        | ClientCommand::PetitionSign { .. }
        | ClientCommand::OfferPetition { .. }
        | ClientCommand::TurnInPetition { .. }
        | ClientCommand::PetitionQuery { .. }
        | ClientCommand::PetitionRename { .. }
        | ClientCommand::PetitionDecline { .. }
        | ClientCommand::TaxiNodeStatusQuery { .. }
        | ClientCommand::TaxiQueryNodes { .. } => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_would_fire_at_the_resume_stays_back() {
        assert!(!paused_allows(&ClientCommand::AttackStop));
        assert!(!paused_allows(&ClientCommand::MountSpecial));
        assert!(!paused_allows(&ClientCommand::CancelAutoRepeat));
    }

    #[test]
    fn the_session_carries_on() {
        assert!(paused_allows(&ClientCommand::Logout));
        assert!(paused_allows(&ClientCommand::SetActiveMover { guid: 1 }));
    }
}
